#!/usr/bin/env python3
"""Every `crates/ocx_*/src/...` literal on a live surface that names nothing on disk.

The crate split moves source paths that files *outside* `crates/` name — rule
files, agent definitions, workflows, taskfiles, acceptance-test docstrings. A
literal naming a path an extraction emptied is not a compile error and no test
reds on it; it simply stops being true, and the next reader follows it.

Two extractions' worth of these were merged before anything looked for them, and
the first version of this sweep missed four more by requiring a file extension,
so it read `…/file_structure/error.rs` and was structurally blind to
`…/file_structure/**`. **A directory literal counts.**

Why this is a script and not a number in a commit message
---------------------------------------------------------
It was a number in a commit message, and the derivation was lost to scratchpad
rotation while the number stayed quoted as a baseline — a standing figure nobody
could reproduce, which is exactly the defect this repository's rulings exist to
refuse. The scope, the exemptions and the floor are all here so that any run
answers the same question.

Exemptions are earned by what a file *is*, never by naming a path
------------------------------------------------------------------
- The frozen pre-split classification baseline. Its rows are a *pre-move*
  reading by construction; a live path in it would mean it had been regenerated.
- The DEC-24 relocation bridge under `crates/ocx_cli/src/exit/`, whose entire
  job is to carry yesterday's spellings alongside today's.
- Files whose subject is constructed test data: the gate tooling's own fixtures
  mint paths like `crates/ocx_lib/src/thing.rs` that were never meant to exist.

Usage
-----
    python3 scripts/dead_path_sweep.py            # list, exit 1 if any
    python3 scripts/dead_path_sweep.py --count    # just the number

Tolerated survivors are named in `BASELINE_DEAD`, not counted. A count
reconciles a survivor that left against one that arrived and reports the same
number either way, so it cannot tell a clean tree from a swap.
"""

from __future__ import annotations

import argparse
import collections
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

#: Surfaces a reader acts on. `.claude/artifacts/` is deliberately absent — a
#: planning artefact is a record of what was true when it was written, and
#: rewriting its paths would falsify the record.
LIVE_PREFIXES = (
    "test/",
    "crates/",
    ".github/",
    "scripts/",
    "taskfiles/",
    "website/",
    ".claude/rules/",
    ".claude/tests/",
    ".claude/skills/",
    ".claude/agents/",
    ".claude/hooks/",
)
LIVE_FILES = {"CLAUDE.md", "taskfile.yml", "Cargo.toml", "deny.toml", ".licenserc.toml"}

#: Trailing punctuation a path picks up from prose, markdown and doc comments.
TRAILING = ".,;:`)*\"'"

#: A path anywhere under some crate, file **or** directory, glob included.
#:
#: Two holes closed at once, both of them the same mistake — a reader narrower
#: than its subject, reporting clean because it never looked. `*` outside the
#: character class truncated `crates/ocx_x/src/**/*.rs` to its first three
#: segments, which the crate-root skip then dropped, so every `paths:` glob and
#: every taskfile `sources:` entry in the repository was unread. And requiring
#: `/src/` hid `crates/ocx_x/Cargo.toml` and the whole `crates/ocx_x/tests/`
#: subtree, which the split moved exactly as often as it moved sources.
LITERAL = re.compile(r"crates/ocx_[a-z_]+(?:/[A-Za-z0-9_./*-]*)?")

#: The same path written as adjacent string literals joined by `pathlib`'s `/`.
#:
#: A third, distinct blindness, and not a truncation like the two above: in
#: `parents[2] / "crates" / "ocx_lib" / "src" / "setup" / "shims.rs"` **no single
#: token resembles a repo path**, so `LITERAL` has nothing to match and the
#: sweep reported clean over a swept tree while CI opened the file and raised
#: `FileNotFoundError`. A join whose first segment is `crates` and whose second
#: names a crate is a repo path, and is judged as one.
SEGMENTED = re.compile(r"""["']crates["']((?:\s*/\s*["'][A-Za-z0-9_.*+-]+["'])+)""")
_SEGMENT = re.compile(r"""["']([A-Za-z0-9_.*+-]+)["']""")

EXEMPT_EXACT = {"crates/ocx_cli/src/exit/classify_baseline_7adaea62.json"}
EXEMPT_PATTERNS = (
    re.compile(r"crates/ocx_cli/src/exit(\.rs|/ocx_[a-z_]+\.rs)$"),  # DEC-24 bridge
    re.compile(r"scripts/(lint_ratchet|test_diff_guard|dead_path_sweep)\.py$"),
    re.compile(r"crates/ocx_test_support/tests/workspace_structure\.rs$"),
    re.compile(r"\.claude/tests/test_hooks\.py$"),
    #: Vendored verbatim from ocx-sh/indexbot, byte-for-byte, and checked
    #: against that repository by `task test:index-conformance-drift`. A path
    #: literal in here is upstream's prose, not a claim this repository makes,
    #: so the sweep has nothing to sweep: the only edit that would satisfy it
    #: is the hand-edit the drift gate exists to forbid — and which it caught
    #: (`crates/ocx_lib's Tag::is_reserved` survived the crate split inside a
    #: file nobody may touch). Exempting the file is what keeps the two gates
    #: from demanding opposite things; correcting the prose is a pull request
    #: against ocx-sh/indexbot.
    re.compile(r"tests/fixtures/index_wire/.*\.json$"),
)

#: Below this the reader has plainly stopped early, and "no dead literals" would
#: be indistinguishable from "read nothing".
MIN_FILES_READ = 200

#: Inherited debt, not a target. Every row predates the sweep and each was read
#: and left deliberately; they live here rather than in the taskfile so that
#: changing the set is a reviewable edit to the file that explains it, not a
#: flag someone widens in passing. It only ever shrinks.
#:
#: Named, not counted. A count reconciles a survivor that left against one that
#: arrived and reports the same total either way — the identity of the survivors is
#: the thing being tolerated, and a total cannot express it. (The same ruling
#: the acceptance suite's census already runs on: diff names, never totals.)
BASELINE_DEAD = frozenset({
    "crates/ocx_cli/src/app/conventions.rs",
    "crates/ocx_cli/src/cli.rs",
    "crates/ocx_cli/src/command/package_info.rs",
    "crates/ocx_cli/src/command/run.rs",
    "crates/ocx_lib/src/cli/exit_code.rs",
    "crates/ocx_lib/src/tls.rs",
    "crates/ocx_store/src/shims/ocx-shim-",
})


def _exempt(rel: str) -> bool:
    return rel in EXEMPT_EXACT or any(p.search(rel) for p in EXEMPT_PATTERNS)


def _live(rel: str) -> bool:
    return rel.startswith(LIVE_PREFIXES) or rel in LIVE_FILES


def _alive(root: Path, candidate: str) -> bool:
    """Whether the literal still names something — DEC-76's instrument.

    A glob is judged by whether it matches anything, never by whether the
    pattern text exists as a filename. `exists()` on a glob is always False,
    which would report every live `paths:` entry in the repository as dead;
    dropping globs instead reports every dead one as live. Only matching them
    tells the two apart.
    """
    if "*" in candidate:
        return any(root.glob(candidate))
    return (root / candidate).exists()


def _segmented_candidates(text: str) -> set[str]:
    """Paths spelled as `"crates" / "ocx_x" / …`, rebuilt into what they name.

    Anchored on the crate segment, not on the join: only a second segment
    matching a crate name makes this a repo path rather than any other use of
    the word. Every other `"crates"` in the tree derives the crate from a glob
    or an index into `parts`, so a derived path stays invisible here — correctly,
    since it cannot go stale.
    """
    out: set[str] = set()
    for match in SEGMENTED.finditer(text):
        parts = _SEGMENT.findall(match.group(1))
        if parts and re.fullmatch(r"ocx_[a-z_]+", parts[0]):
            out.add("crates/" + "/".join(parts))
    return out


def _declared_artifacts(root: Path, candidates: list[str]) -> set[str]:
    """Of `candidates`, the ones `.gitignore` declares are build outputs.

    A generated path is absent from a clean tree by design, so asking whether
    it exists is a category error — `crates/ocx_cli/ocx.cdx.json` is written by
    `task sbom:generate:json` and named by the two files that consume it. The
    answer is read off the repository's own `.gitignore` rather than listed
    here, because an exemption this script names is an exemption that goes
    stale the way everything else in this file has.
    """
    if not candidates:
        return set()
    # `check-ignore` exits 1 when nothing matched, which is the common case.
    done = subprocess.run(
        ["git", "-C", str(root), "check-ignore", "--stdin"],
        input="\n".join(candidates), capture_output=True, text=True, encoding="utf-8",
        check=False,
    )
    return set(done.stdout.split())


def sweep(root: Path = ROOT) -> tuple[dict[str, set[str]], int]:
    """Dead literals mapped to the files naming them, and the count of files read."""
    tracked = subprocess.run(
        ["git", "-C", str(root), "ls-files"],
        capture_output=True, text=True, encoding="utf-8", check=True,
    ).stdout.split()

    dead: dict[str, set[str]] = collections.defaultdict(set)
    read = 0
    for rel in tracked:
        if not _live(rel) or _exempt(rel):
            continue
        try:
            text = (root / rel).read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        read += 1
        candidates = {match.rstrip(TRAILING).rstrip("/") for match in LITERAL.findall(text)}
        candidates |= _segmented_candidates(text)
        for candidate in candidates:
            if not _alive(root, candidate):
                dead[candidate].add(rel)
    for artifact in _declared_artifacts(root, sorted(dead)):
        del dead[artifact]
    return dead, read


def self_test() -> int:
    """Every shape this sweep decides, shown red and green on a tree we build.

    Without this the script had no way to fail on purpose, which is the same
    defect it exists to catch one level down: a green here was indistinguishable
    from a sweep that read nothing. It was also wired to no taskfile at all, so
    the only thing running it was a human remembering to.
    """
    checks = 0
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        (root / "crates/ocx_util/src").mkdir(parents=True)
        (root / "crates/ocx_util/src/live.rs").write_text("pub fn x() {}\n", encoding="utf-8")
        (root / "crates/ocx_util/src/subdir").mkdir()
        (root / "crates/ocx_util/src/subdir/k.rs").write_text("\n", encoding="utf-8")
        (root / "crates/ocx_util/Cargo.toml").write_text("[package]\n", encoding="utf-8")
        (root / ".claude/rules").mkdir(parents=True)
        (root / "scripts").mkdir()
        (root / ".gitignore").write_text("*.cdx.json\n", encoding="utf-8")

        def write(rel: str, text: str) -> None:
            (root / rel).write_text(text, encoding="utf-8")

        # A live file literal, a live DIRECTORY literal (the DEC-52 blind spot),
        # a dead file literal, a dead directory literal, and the crate root,
        # which is judged like anything else and happens to be live.
        write(".claude/rules/subsystem-x.md",
              "see `crates/ocx_util/src/live.rs` and `crates/ocx_util/src/subdir/**`\n"
              "and `crates/ocx_util/src/gone.rs` and `crates/ocx_util/src/nodir/**`\n"
              "and `crates/ocx_util/src` which is the crate root\n")
        # An INTERIOR glob, live and dead. `exists()` answers False for both, so
        # only matching the pattern tells them apart — the hole that made every
        # `paths:` and `sources:` entry in the repository unreadable.
        write(".claude/rules/subsystem-glob.md",
              "paths: `crates/ocx_util/src/**/*.rs` and `crates/ocx_util/src/**/*.toml`\n")
        # OUTSIDE `src/`, live and dead. A manifest and a `tests/` tree move in
        # an extraction exactly as often as a source file does.
        write(".claude/rules/subsystem-manifest.md",
              "see `crates/ocx_util/Cargo.toml` and `crates/ocx_gone/Cargo.toml`\n")
        # A path `.gitignore` declares a build output: absent by design, so
        # asking whether it exists is a category error.
        write("scripts/emit.sh", "# writes crates/ocx_util/ocx.cdx.json\n")
        # SEGMENTED: the same path as adjacent literals joined by `/`, live and
        # dead — no token here resembles a repo path, which is why `LITERAL`
        # could not see the `shell_latency.py` open that CI raised on. The third
        # line derives the crate from a variable and must stay invisible: a
        # derived path cannot go stale, and claiming it would report every
        # glob-driven walk in the tree.
        write("scripts/bench.py",
              'SRC = base / "crates" / "ocx_util" / "src" / "live.rs"\n'
              'GONE = base / "crates" / "ocx_util" / "src" / "vanished.rs"\n'
              'ANY = base / "crates" / name / "src"\n')
        # Off a live prefix entirely: an artifact records what was true when
        # written, so its dead literal must NOT be reported.
        (root / ".claude/artifacts").mkdir()
        write(".claude/artifacts/old_plan.md", "`crates/ocx_util/src/ancient.rs`\n")
        # Exempt by what the file IS, not by naming a path.
        write("scripts/lint_ratchet.py", "# crates/ocx_util/src/fixture_only.rs\n")

        subprocess.run(["git", "-C", str(root), "init", "-q"], check=True)
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)

        dead, read = sweep(root)
        expect(read >= 3, f"the reader saw {read} live file(s), so nothing below it means anything")
        checks += 1
        expect(
            sorted(dead) == [
                "crates/ocx_gone/Cargo.toml",
                "crates/ocx_util/src/**/*.toml",
                "crates/ocx_util/src/gone.rs",
                "crates/ocx_util/src/nodir",
                "crates/ocx_util/src/vanished.rs",
            ],
            f"RED shapes wrong — a dead file, DIRECTORY, GLOB, MANIFEST and SEGMENTED join, "
            f"nothing else: {sorted(dead)}",
        )
        checks += 1
        for green in (
            "crates/ocx_util/src/live.rs",          # live file
            "crates/ocx_util/src/subdir",           # live directory
            "crates/ocx_util/src",                  # crate root
            "crates/ocx_util/src/**/*.rs",          # live interior glob
            "crates/ocx_util/Cargo.toml",           # live, outside src/
            "crates/ocx_util/ocx.cdx.json",         # declared build output
            "crates/ocx_util/src/ancient.rs",       # off a live prefix
            "crates/ocx_util/src/fixture_only.rs",  # exempt by what the file is
        ):
            expect(green not in dead, f"GREEN shape reported as dead: {green}")
        checks += 1
        # The join reader, on its own terms: it claims a literal crate segment
        # and nothing else. A derived one cannot go stale, and claiming it would
        # report every glob-driven walk in the tree as a path.
        expect(
            _segmented_candidates('base / "crates" / "ocx_util" / "src" / "x.rs"')
            == {"crates/ocx_util/src/x.rs"},
            "the join reader does not rebuild a literal crate segment",
        )
        expect(
            _segmented_candidates('base / "crates" / name / "src"') == set(),
            "the join reader claimed a path whose crate comes from a variable",
        )
        checks += 1

    # The reader floor, shown reachable: an empty repository must refuse rather
    # than report a clean tree.
    with tempfile.TemporaryDirectory() as td:
        empty = Path(td)
        subprocess.run(["git", "-C", str(empty), "init", "-q"], check=True)
        _, read = sweep(empty)
        expect(read == 0, f"the empty tree read {read} files")
        expect(read < MIN_FILES_READ, "the floor cannot fire on an empty tree")
    checks += 1

    # The verdict, on a swap a count cannot see. One tolerated survivor fixed,
    # one new literal arrived: the total is unchanged, and that was the whole
    # defect — so the case is built at exactly equal cardinality.
    one_out = next(iter(sorted(BASELINE_DEAD)))
    swapped = (set(BASELINE_DEAD) - {one_out}) | {"crates/ocx_util/src/arrived.rs"}
    expect(len(swapped) == len(BASELINE_DEAD), "the swap case must not change the count")
    lines = verdict(swapped)
    expect(len(lines) == 2, f"a swap must red on BOTH directions, got {len(lines)}: {lines}")
    expect(
        any("crates/ocx_util/src/arrived.rs" in line for line in lines),
        "the arrival is not named in the verdict",
    )
    expect(any(one_out in line for line in lines), "the departure is not named in the verdict")
    checks += 1

    expect(verdict(set(BASELINE_DEAD)) == [], "the baseline itself must be green")
    expect(
        len(verdict(set(BASELINE_DEAD) | {"crates/ocx_x/src/new.rs"})) == 1,
        "an arrival alone must red exactly once",
    )
    expect(len(verdict(set(BASELINE_DEAD) - {one_out})) == 1, "a departure alone must red once")
    checks += 1

    print(f"dead-path sweep self-test: {checks} checks passed")
    return 0


def expect(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"dead-path sweep self-test: {message}")


def verdict(dead: set[str]) -> list[str]:
    """Both directions the baseline can be wrong, as lines to print on stderr.

    Arrivals red for the obvious reason. Departures red because a tolerated
    path that is no longer dead tolerates nothing — the same shape as an
    exclusion whose target the scan can no longer find, which reads as coverage
    and is not. Fixing one is a two-line change: the fix, and the row.
    """
    problems = []
    if arrived := sorted(dead - BASELINE_DEAD):
        problems.append(
            "dead-path sweep: literal(s) not in the baseline — each names a path "
            f"nothing on disk provides: {', '.join(arrived)}"
        )
    if departed := sorted(BASELINE_DEAD - dead):
        problems.append(
            "dead-path sweep: BASELINE_DEAD still tolerates literal(s) the sweep no "
            "longer finds; a tolerance whose target is gone tolerates nothing and "
            f"reads as debt — delete the row(s): {', '.join(departed)}"
        )
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--count", action="store_true", help="print only the count")
    parser.add_argument("--self-test", action="store_true", help="show the sweep red and green")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    dead, read = sweep()

    if read < MIN_FILES_READ:
        print(
            f"dead-path sweep: read {read} files, below the floor of {MIN_FILES_READ} — "
            "refusing to report a result, because a reader that stopped early and a "
            "clean tree look identical here",
            file=sys.stderr,
        )
        return 2

    problems = verdict(set(dead))

    if args.count:
        print(len(dead))
        return 1 if problems else 0

    for path in sorted(dead):
        print(f"{path}\n    named by: {', '.join(sorted(dead[path]))}")
    print(
        f"dead-path sweep: {len(dead)} dead literal(s) over {read} live file(s), "
        f"{len(BASELINE_DEAD)} tolerated by name"
    )

    for line in problems:
        print(line, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
