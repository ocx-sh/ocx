#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Decide what `task verify:scoped` runs, and write the verify mark it reads back.

    scripts/scoped_gate.py --plan                          # JSON, consumed by the task
    scripts/scoped_gate.py --mark full|scoped [--crates <crate>...]
    scripts/scoped_gate.py --mark-precheck                 # task verify's first step (C-017)
    scripts/scoped_gate.py --record-escapes --junit-dir <dir>  # bazel:test:accept, failed, merging
    scripts/scoped_gate.py --self-test

The gate (plan_crate_split_workspace.md C-019, C-020, C-021):

  1. base = the `head` of the last *full* verify mark when it is an ancestor of
     HEAD, else `git merge-base origin/main HEAD`. Changed paths are
     `git diff --name-only <base>...HEAD` plus the working tree (tracked
     modifications and untracked files).
  2. Non-member paths route. `.agents/**` (swarm memory), `CLAUDE.md` and the
     `.claude/` subtrees
     whose only gate is `claude:tests` (CLAUDE_TESTS_READS, a permit list —
     any other `.claude/` path escalates, `.claude/taskfile.yml` included,
     so a new subtree is loud until someone routes it) → `task
     claude:tests`;
     `.github/**` → actionlint plus `.claude/tests/test_workflows.py`; an
     existing `test/tests/test_<f>.py` → `task test:parallel --
     tests/test_<f>.py`; `scripts/**` → `task scripts:self-test` AND the full
     verify (the gate tooling decides what every other gate runs, so a change
     there is never certified by less than the full run — the route is what
     `--plan` reports, the escalation is what runs it). Every other
     non-member path — root manifests, the lockfile, every taskfile,
     `test/src/**`, `test/conftest.py`, `test/tests/` helpers, `external/**`,
     the nextest floor files under `crates/` — escalates to `task verify` and
     is printed as the reason.
  3. `crates/<dir>/Cargo.toml` and `crates/<dir>/README.md` are workspace
     structure rather than that crate's code, so they route to the
     workspace-structure guards (`cargo nextest run -p ocx_test_support
     --test workspace_structure`) instead of resolving to their own package.
     Every other member path (`crates/<dir>/...`) maps to a package name
     through `cargo metadata`, so `crates/ocx_cli/` is the package `ocx`.
  4. A changed crate escalates when it is a hub: at least HUB_RDEPS reverse
     dependents inside the workspace (counted from the full `cargo metadata`
     resolve graph every run, dev-dependencies included) or a member of
     ECOSYSTEM. A crate whose `test:scoped` row reads `escalate`
     (TABLE_ESCALATES) escalates too, before the per-crate steps are spent.
  5. Otherwise the decision is `scoped` (the task runs the per-crate steps and
     `test:scoped`) or `routed` (nothing under `crates/` changed).

The mark, `.claude/hooks/.state/commit-verified`, is JSON:
`{"timestamp": <epoch>, "head": "<sha>", "scope": "full"|"scoped",
"crates": [...], "toplevel": "<working tree>", "tree": "<git write-tree>"}`.
`tree` is the real index's tree, read by `commit_gate.py`'s merge-commit
clause (C-017), and is recorded only when the run proved it built that tree:
the working tree equalled the index at `--mark-precheck` and at `--mark`, with
the index unchanged between (`proven_tree`); otherwise it is null. While
`MERGE_HEAD` exists `--mark` refuses instead (AM-8).
`--mark scoped --log-run` (verify:scoped only) also appends the run to
`<git-common-dir>/ocx-gate/scoped_runs.jsonl`, which `--record-escapes` reads
when T2 fails during a merge (C-018, ADR C-ESC). A scoped mark also carries
`full_head`, the head of the last full mark, so the base of step 1 survives any
number of scoped runs. `scripts/commit_gate.py` is the reader, run by git
itself through the hooks `task git:hooks` installs; a bare integer (the retired `echo $(date +%s)`
stamp) is not a mark, and neither is one with no `toplevel`. Where the file
lives is `mark_file()`, and it is the reader's `.claude/`, not this script's —
see there for why an agent worktree made C-021 void without anything looking
wrong, and see `worktree_id` for why one shared home needs the writer named.

Stdlib only. `--self-test` drives the path→crate map, the routing table and
the hub predicate over a fixture metadata document, the mark round-trip over a
throwaway repository, `--mark`'s choice of file from a real linked worktree of
a throwaway project, and shows that a failing `cargo metadata` is a loud exit
— never an empty crate set.
"""

from __future__ import annotations

import argparse
import ast
import contextlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import xml.etree.ElementTree as ET
from dataclasses import asdict, dataclass, field
from fnmatch import fnmatchcase
from pathlib import Path

from _git import git

REPO_ROOT = Path(__file__).resolve().parents[1]


def mark_file() -> Path:
    """Where the verify mark lives — the file the pre-commit hook reads.

    NOT `REPO_ROOT`. The reader builds `.state/` from `CLAUDE_PROJECT_DIR`,
    which Claude Code sets to the session's checkout and keeps there even when
    the cwd is `.agents/worktrees/<slug>`; that shared mark home is deliberate,
    so the reader is right and this writer is the half that drifted. Hanging the
    mark off `REPO_ROOT` put it in whatever worktree the script happens to be
    checked out in, and the failure has the worst possible shape: `--mark`
    PRINTS the JSON it wrote, so the stamp looks like it worked, while the
    gate decides on a file that stamp never touched — a success
    indistinguishable from never having run. C-021's "a mark certifies the
    HEAD it was written on" is then void in both directions: a stale mark
    blocks a verified commit, and a mark left by an unrelated HEAD passes one.

    Deliberately not `git rev-parse --git-common-dir`: this repository's four
    sibling checkouts (`goat`, `evelynn`, `sion`, `soraka`) share one common
    dir and each verifies its own HEAD, so a mark resolved that way would
    make them thrash over a single file. `REPO_ROOT` stays right for what it
    is actually for — locating `scripts/` and the crate map — and
    is the fallback when no harness set `CLAUDE_PROJECT_DIR`, which is the
    shape a plain CLI or CI invocation has.
    """
    project_dir = os.environ.get("CLAUDE_PROJECT_DIR")
    home = Path(project_dir) if project_dir and (Path(project_dir) / ".claude").is_dir() else REPO_ROOT
    return home / ".claude" / "hooks" / ".state" / "commit-verified"

# MACHINE_READ held the one artifact a gate script read as input —
# `edge_inventory.py`'s FILE_MAP — and routed it to the full verify because
# `rust:deps:inventory` was the only consumer. Both left with `ocx_lib` at
# WP-37. Restore the constant, not a hard-coded path, the day another gate
# script reads an artifact: it was imported from its consumer on purpose, so a
# rename there moved the route with it.
# The .claude/ subtrees whose only gate is `claude:tests`: nothing else —
# no cargo target, no task, no gate script — consumes them, so that run
# certifies a change there. A permit list: a path under .claude/ outside it
# escalates. `.claude/scripts/` is out — no gate exercises what it holds,
# and a route to `claude:tests` would certify nothing.
CLAUDE_TESTS_READS = (
    ".claude/agents/",
    ".claude/artifacts/",
    ".claude/hooks/",
    ".claude/rules/",
    ".claude/rules.md",
    ".claude/settings.json",
    ".claude/skills/",
    ".claude/templates/",
    ".claude/tests/",
)
WORKFLOW_TEST = Path(".claude") / "tests" / "test_workflows.py"
# A crate directory's workspace-structure surface: what it declares to the
# workspace rather than what it compiles. `crates/<crate>/Cargo.toml` carries
# the dependency edges C-009 constrains and the feature sets C-019 does;
# `crates/<crate>/README.md` carries the may-depend-on rows C-046 checks.
MANIFEST_FILES = frozenset({"Cargo.toml", "README.md"})

# C-019 (4): the ecosystem tier of the ADR crate map. Any change here is a
# change every satellite links, so the scoped gate never certifies it.
ECOSYSTEM = frozenset(
    {
        "ocx_util",
        "ocx_console",
        "ocx_oci",
        "ocx_trust",
        "ocx_sign",
        "ocx_config",
        "ocx_index",
        "ocx_package",
        "ocx_python",
    }
)
HUB_RDEPS = 4
# The `[crates]` rows of test/scoped_rows.toml that read `escalate`: their
# acceptance subset is the whole suite, so the gate escalates before spending
# the per-crate steps. Restated here for the `verify:scoped` summary's reader;
# `--self-test` and `--check-coverage` both hold it equal to the table. `ocx`
# left at WP-06 (C-014): its command files route by `command` markers now.
# `ocx_python` has no acceptance subset: `ocx` does not link it.
TABLE_ESCALATES = frozenset({"ocx_test_support", "ocx_python"})
ROOT_TASKFILE = REPO_ROOT / "taskfile.yml"

# C-012 — the selection table. Read here and nowhere else.
ROWS_FILE = REPO_ROOT / "test" / "scoped_rows.toml"
ROWS_SECTIONS = frozenset({"crates", "verbs", "security"})
# The CLI crate: package `ocx`, routed by markers (C-013/C-014) instead of a row.
CLI_PACKAGE = "ocx"
CLI_DIR = "crates/ocx_cli/"
# The lint tier's floor and ceilings: read only by `test:lint:structure`.
LINT_TIER_FILES = ("test/LINT_FLOOR", "test/LINT_SKIP_CEILING", "test/LINT_XFAIL_CEILING")
COMMAND_DIR = "crates/ocx_cli/src/command/"
# C-015 reader floors, each independent of the glob it checks: the command
# files are counted off the tree against this constant, the acceptance modules
# against `bazel_gate_proofs.ACCEPTANCE_MODULE_TARGETS` (read with `ast`).
COMMAND_FILES_FLOOR = 80
GATE_PROOFS = REPO_ROOT / "scripts" / "bazel_gate_proofs.py"
HEX_MEMORY = REPO_ROOT / ".agents" / "memory" / "hex.md"
# The ADR C-ROWS `[security]` list, AM-3's seven command files and the WP-06
# security review's additions: a floor the table may exceed and never drop below. Without it the per-glob self-test walks
# whatever the table lists, and deleting a row deletes its own red.
REQUIRED_SECURITY = (
    ".github/workflows/**",
    ".github/actions/**",
    "crates/ocx_oci/**",
    "crates/ocx_trust/**",
    "crates/ocx_config/**",
    "crates/ocx_store/**",
    "crates/ocx_sign/**",
    "crates/ocx_cli/src/command/package_sign*.rs",
    "crates/ocx_cli/src/command/package_verify.rs",
    "crates/ocx_cli/src/command/package_attest.rs",
    "crates/ocx_cli/src/command/login.rs",
    "crates/ocx_cli/src/command/logout.rs",
    "crates/ocx_cli/src/command/package_sbom.rs",
    "crates/ocx_cli/src/command/shell_allow.rs",
    "crates/ocx_cli/src/command/shell_revoke.rs",
    "crates/ocx_cli/src/command/self_group/update.rs",
    "crates/ocx_cli/src/command/config_push.rs",
    "crates/ocx_cli/src/command/config_update.rs",
    "crates/ocx_cli/src/command/config_setup.rs",
    # WP-06 security review: the fail-closed signature enforcement on
    # install/pull, and the code that reads or completes a consent / rc-file
    # write rather than only the verbs that grant one.
    "crates/ocx_package_manager/src/tasks/auto_verify.rs",
    "crates/ocx_package_manager/src/tasks/verify.rs",
    "crates/ocx_project/src/consent.rs",
    "crates/ocx_cli/src/command/shell_state.rs",
    "crates/ocx_cli/src/command/self_group/activate.rs",
    "crates/ocx_cli/src/command/self_group/setup.rs",
    # `self_update` replaces the running binary, and no acceptance module that
    # exercises it carries a marker any [crates] row would select.
    "crates/ocx_package_manager/src/tasks/update_check.rs",
)


@dataclass(slots=True, frozen=True)
class Rows:
    """test/scoped_rows.toml, validated."""

    crates: dict[str, list[str] | str]  # package → globs relative to test/, or "escalate"
    verbs: dict[str, str]  # pattern under command/ → "escalate"
    security: list[str]  # repo-relative globs that always escalate


def load_rows(path: Path = ROWS_FILE) -> Rows:
    """The table, or a loud SystemExit (exit 1) naming the parse error.

    Never a silent escalate (C-012): a table that cannot be read is a gate that
    cannot decide, and an escalate would hide that behind a slow green.
    """
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as err:
        raise SystemExit(f"scoped gate: cannot read {path}: {err}") from err
    except tomllib.TOMLDecodeError as err:
        raise SystemExit(f"scoped gate: {path} is malformed: {err}") from err

    problems: list[str] = []

    def strings(value: object) -> bool:
        return isinstance(value, list) and bool(value) and all(isinstance(v, str) and v for v in value)

    if set(data) != ROWS_SECTIONS:
        problems.append(f"sections {sorted(data)} != {sorted(ROWS_SECTIONS)}")
    crates = data.get("crates", {})
    verbs = data.get("verbs", {})
    security = data.get("security", {})
    if not isinstance(crates, dict) or not crates:
        problems.append("[crates] must be a non-empty table")
        crates = {}
    for crate, row in crates.items():
        if crate == CLI_PACKAGE:
            problems.append(f"[crates] {crate}: the CLI crate routes by `command` markers, never by a row")
        elif row != "escalate" and not strings(row):
            problems.append(f"[crates] {crate}: must be a non-empty list of globs or \"escalate\", got {row!r}")
    if not isinstance(verbs, dict):
        problems.append("[verbs] must be a table")
        verbs = {}
    for pattern, value in verbs.items():
        if value != "escalate":
            problems.append(f"[verbs] {pattern!r}: the only value is \"escalate\", got {value!r}")
    if not isinstance(security, dict) or set(security) != {"escalate"} or not strings(security["escalate"]):
        problems.append("[security] must hold exactly `escalate = [<glob>, ...]`, non-empty")
        security = {"escalate": []}
    if problems:
        raise SystemExit(f"scoped gate: {path} is malformed: " + "; ".join(problems))
    return Rows(crates=dict(crates), verbs=dict(verbs), security=list(security["escalate"]))


def _command_call(node: ast.AST) -> bool:
    """Any `<...>.mark.command(...)` / `mark.command(...)` call, however spelled."""
    if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "command"):
        return False
    owner = node.func.value
    return (isinstance(owner, ast.Attribute) and owner.attr == "mark") or (
        isinstance(owner, ast.Name) and owner.id == "mark"
    )


def _pytest_command(node: ast.AST) -> bool:
    """Exactly `pytest.mark.command(...)` — the one spelling C-013 routes on."""
    return (
        _command_call(node)
        and isinstance(node.func.value, ast.Attribute)  # type: ignore[attr-defined]
        and isinstance(node.func.value.value, ast.Name)  # type: ignore[attr-defined]
        and node.func.value.value.id == "pytest"  # type: ignore[attr-defined]
    )


def _is_pytestmark(node: ast.AST) -> bool:
    return isinstance(node, ast.Name) and node.id == "pytestmark"


def read_markers(source: str) -> tuple[list[str], list[str]]:
    """(command keys, unreadable-marker problems) of one test module, by `ast`.

    What pytest applies to every test of the module is the *last* module-level
    `pytestmark` binding, so this replays the top-level statements in order:
    `pytestmark = <mark>` or `= [<mark>, ...]` rebinds (a bare `pytestmark`
    inside the list carries the previous binding forward), and
    `pytestmark.append(<mark>)` extends it. A `pytest.mark.command(...)` in the
    surviving binding contributes its string-literal arguments as keys.

    Every other `command` mark is a problem, never a silent skip: one with a
    non-literal, keyword or missing argument, and one this replay cannot credit
    (a decorator, a binding a later one dropped, one nested under `if`, an
    alias of `pytest` or `mark`). Each names a module the author believed was
    routed, and the coverage guard reds on it rather than leave the verb unrun.
    """
    if "command" not in source:
        return [], []
    tree = ast.parse(source)
    marks: list[ast.expr] = []
    for stmt in tree.body:
        if isinstance(stmt, ast.Assign) and any(_is_pytestmark(t) for t in stmt.targets):
            elts = stmt.value.elts if isinstance(stmt.value, (ast.List, ast.Tuple)) else [stmt.value]
            marks = [m for e in elts for m in (marks if _is_pytestmark(e) else [e])]
        elif (
            isinstance(stmt, ast.Expr)
            and isinstance(stmt.value, ast.Call)
            and isinstance(stmt.value.func, ast.Attribute)
            and stmt.value.func.attr == "append"
            and _is_pytestmark(stmt.value.func.value)
            and len(stmt.value.args) == 1
        ):
            marks = [*marks, stmt.value.args[0]]
    keys: list[str] = []
    problems: list[str] = []
    credited: set[int] = set()
    for mark in marks:
        if not _pytest_command(mark):
            continue
        credited.add(id(mark))
        args = mark.args  # type: ignore[attr-defined]
        if mark.keywords or not args or not all(  # type: ignore[attr-defined]
            isinstance(a, ast.Constant) and isinstance(a.value, str) and a.value for a in args
        ):
            problems.append(f"line {mark.lineno}: a command mark takes string-literal keys only: {ast.unparse(mark)}")
            continue
        keys.extend(a.value for a in args)
    for node in ast.walk(tree):
        if _command_call(node) and id(node) not in credited:
            problems.append(
                f"line {node.lineno}: a command mark outside the module's surviving `pytestmark` binding,"  # type: ignore[attr-defined]
                f" or not spelled `pytest.mark.command` — the gate cannot route on it: {ast.unparse(node)}"
            )
    return keys, problems


def module_markers(test_dir: Path) -> dict[str, tuple[list[str], list[str]]]:
    """`tests/test_*.py` (relative to test/) → `read_markers` of it."""
    return {
        f"tests/{path.name}": read_markers(path.read_text(encoding="utf-8"))
        for path in sorted((test_dir / "tests").glob("test_*.py"))
    }


def command_keys(root: Path) -> list[str]:
    """Every `crates/ocx_cli/src/command/**/*.rs`, as its C-013 key (`launcher/exec`)."""
    base = root / COMMAND_DIR
    return sorted(path.relative_to(base).with_suffix("").as_posix() for path in base.rglob("*.rs"))


def security_glob(path: str, rows: Rows) -> str | None:
    """The first `[security]` glob `path` matches. `*` crosses `/` (fnmatch)."""
    return next((glob for glob in rows.security if fnmatchcase(path, glob)), None)


def verbs_pattern(key: str, rows: Rows) -> str | None:
    """The first `[verbs]` pattern a command key's file (`<key>.rs`) matches."""
    return next((pattern for pattern in rows.verbs if fnmatchcase(f"{key}.rs", pattern)), None)


def selected_modules(key: str, markers: dict[str, tuple[list[str], list[str]]]) -> list[str]:
    """The modules whose marker keys match a command key (C-013: keys fnmatch)."""
    return sorted(module for module, (keys, _) in markers.items() if any(fnmatchcase(key, k) for k in keys))


def hex_security_globs(hex_text: str) -> list[str] | None:
    """The `reviewer:security` perspective's `when:` globs in hex.md, or None when unparseable.

    `when: "{a,b,c}"` is a brace list and `when: "a"` a single glob; anything
    else — no `reviewer:security` role, no `when:` after it, an empty or nested
    brace list — is unparseable, and C-015 makes that a red: the invariant it
    feeds would otherwise pass over a list it never read.
    """
    match = re.search(r"role:\s*reviewer:security\s*\n\s*when:\s*\"([^\"\n]*)\"", hex_text)
    if not match:
        return None
    text = match.group(1).strip()
    if text.startswith("{") and text.endswith("}"):
        text = text[1:-1]
    globs = [glob.strip() for glob in text.split(",")]
    if not text or any(not glob or set(glob) & set("{}") for glob in globs):
        return None
    return globs


def gate_module_floor(gate_proofs: Path = GATE_PROOFS) -> int:
    """`ACCEPTANCE_MODULE_TARGETS`, read with `ast` — the module floor no glob here can lower."""
    try:
        tree = ast.parse(gate_proofs.read_text(encoding="utf-8"))
    except (OSError, SyntaxError) as err:
        raise SystemExit(f"scoped gate: cannot read the module floor from {gate_proofs}: {err}") from err
    for stmt in tree.body:
        if (
            isinstance(stmt, ast.Assign)
            and any(isinstance(t, ast.Name) and t.id == "ACCEPTANCE_MODULE_TARGETS" for t in stmt.targets)
            and isinstance(stmt.value, ast.Constant)
            and isinstance(stmt.value.value, int)
        ):
            return stmt.value.value
    raise SystemExit(f"scoped gate: {gate_proofs} declares no integer ACCEPTANCE_MODULE_TARGETS")


def coverage_findings(
    root: Path,
    *,
    rows: Rows,
    markers: dict[str, tuple[list[str], list[str]]],
    members: set[str],
    module_floor: int,
    command_floor: int,
    hex_text: str,
    tracked: list[str],
) -> list[str]:
    """C-015's three invariants and two reader floors over one tree; [] is green.

    Beside the three invariants, the table is held to the tree it routes: a
    `[crates]` row per workspace member but `ocx` and none for a non-member, no
    glob that matches no module, no marker key or `[verbs]` pattern that matches
    no command file, no marker key that is all wildcard (it would name every
    command file and void invariant 2), the escalate rows equal to
    TABLE_ESCALATES, `[security]` a superset of REQUIRED_SECURITY, and every
    `[security]` glob matching a tracked path — a glob a rename left matching
    nothing escalates nothing while still reading as protection.
    """
    findings: list[str] = []
    modules = sorted(markers)
    keys = command_keys(root)

    if len(modules) < module_floor:
        findings.append(
            f"read {len(modules)} acceptance modules, below the floor of {module_floor}"
            " (bazel_gate_proofs.ACCEPTANCE_MODULE_TARGETS) — the module reader stopped early"
        )
    if len(keys) < command_floor:
        findings.append(
            f"read {len(keys)} command files under {COMMAND_DIR}, below the floor of {command_floor}"
            " (COMMAND_FILES_FLOOR) — the command reader stopped early"
        )

    expected_rows = members - {CLI_PACKAGE}
    for crate in sorted(expected_rows - set(rows.crates)):
        findings.append(f"[crates] has no row for workspace member {crate}")
    for crate in sorted(set(rows.crates) - expected_rows):
        findings.append(f"[crates] row {crate} names no workspace member")
    escalates = {crate for crate, row in rows.crates.items() if row == "escalate"}
    if escalates != set(TABLE_ESCALATES):
        findings.append(f"[crates] escalate rows {sorted(escalates)} != scoped_gate.TABLE_ESCALATES {sorted(TABLE_ESCALATES)}")
    globs = [glob for row in rows.crates.values() if row != "escalate" for glob in row]
    for crate, row in sorted(rows.crates.items()):
        if row == "escalate":
            continue
        for glob in row:
            if not any(fnmatchcase(module, glob) for module in modules):
                findings.append(f"[crates] {crate} glob {glob} matches no acceptance module")

    # Invariant 1: every module is reached by a glob or a marker.
    for module in modules:
        module_keys, problems = markers[module]
        for problem in problems:
            findings.append(f"test/{module}: unreadable command marker — {problem}")
        if not module_keys and not problems and not any(fnmatchcase(module, glob) for glob in globs):
            findings.append(
                f"test/{module}: matched by no [crates] glob and carries no `command` marker"
                " — no scoped run can ever select it"
            )
        for key in module_keys:
            if not _key_shape_ok(key, keys):
                findings.append(
                    f"test/{module}: marker key {key!r} — a key is a command key, or one followed by a"
                    " single trailing `*` (`package_push*`, `self_group/*`); a wider pattern names files"
                    " nobody reviewed it for"
                )
            elif not any(fnmatchcase(command, key) for command in keys):
                findings.append(f"test/{module}: marker key {key!r} matches no file under {COMMAND_DIR}")

    # Invariant 2: every command file is named by a marker, [verbs] or [security].
    for key in keys:
        path = f"{COMMAND_DIR}{key}.rs"
        if security_glob(path, rows) or verbs_pattern(key, rows) or selected_modules(key, markers):
            continue
        findings.append(f"{path}: named by no `command` marker, [verbs] entry or [security] glob")
    for pattern in rows.verbs:
        if not any(fnmatchcase(f"{key}.rs", pattern) for key in keys):
            findings.append(f"[verbs] {pattern!r} matches no file under {COMMAND_DIR}")

    # Invariant 3: [security] covers what the security reviewer is summoned for,
    # and never drops below the ADR list (+ AM-3).
    hex_globs = hex_security_globs(hex_text)
    if hex_globs is None:
        findings.append("hex.md: the reviewer:security `when:` glob list is missing or unparseable")
    else:
        for glob in hex_globs:
            if glob not in rows.security:
                findings.append(f"[security] lacks {glob}, a reviewer:security `when:` glob in hex.md")
    for glob in REQUIRED_SECURITY:
        if glob not in rows.security:
            findings.append(f"[security] lacks {glob}, which the ADR C-ROWS list / AM-3 requires")
    for glob in rows.security:
        if not any(fnmatchcase(path, glob) for path in tracked):
            findings.append(f"[security] {glob} matches no tracked file — a rename left it guarding nothing")
    return findings


def _key_shape_ok(key: str, commands: list[str]) -> bool:
    """A marker key is a command key, `<command key>*`, `<command key>_*` or `<directory>/*`.

    Anything wider (`p*`, `*_*`, `?ull`) lets one module claim files nobody
    reviewed it for, and — `*` crossing `/` — claim them silently as the tree
    grows. The one wildcard allowed is a single trailing `*` after a prefix
    that is itself a command key or ends a key segment.
    """
    if not set(key) & set("*?[]"):
        return True
    base = key[:-1]
    return (
        key.endswith("*")
        and not set(base) & set("*?[]")
        and (
            base in commands
            or (base.endswith("_") and base.rstrip("_") in commands)
            or (base.endswith("/") and any(command.startswith(base) for command in commands))
        )
    )


def live_coverage(root: Path) -> list[str]:
    """`coverage_findings` over a checkout, with the live floors."""
    rows = load_rows(root / "test" / "scoped_rows.toml")
    members = set(parse_workspace(cargo_metadata(root)).rdeps)
    hex_path = root / ".agents" / "memory" / "hex.md"
    try:
        hex_text = hex_path.read_text(encoding="utf-8")
    except OSError as err:
        return [f"hex.md: cannot read {hex_path}: {err}"]
    return coverage_findings(
        root,
        rows=rows,
        markers=module_markers(root / "test"),
        members=members,
        module_floor=gate_module_floor(root / "scripts" / "bazel_gate_proofs.py"),
        command_floor=COMMAND_FILES_FLOOR,
        hex_text=hex_text,
        tracked=git(root, "ls-files").splitlines(),
    )

# ---------------------------------------------------------------------------
# Workspace (cargo metadata)
# ---------------------------------------------------------------------------


@dataclass(slots=True)
class Workspace:
    """What the gate needs from `cargo metadata`."""

    crate_of_dir: dict[str, str]  # "crates/<dir>" → package name
    rdeps: dict[str, int]  # package name → reverse dependents among members
    # Every member's `custom-build` target, repo-relative: a manifest may name
    # any file (`build = "gen/main.rs"`), so the file name alone cannot find them.
    build_scripts: frozenset[str] = frozenset()

    def hubs(self) -> list[str]:
        """Crates the scoped gate never certifies: by reverse-dependent count or tier."""
        return sorted(name for name, n in self.rdeps.items() if n >= HUB_RDEPS or name in ECOSYSTEM)

    def hub_reasons(self, crate: str) -> list[str]:
        reasons = []
        if crate in ECOSYSTEM:
            reasons.append("ecosystem tier")
        if self.rdeps.get(crate, 0) >= HUB_RDEPS:
            reasons.append(f"{self.rdeps[crate]} reverse dependents (hub at {HUB_RDEPS})")
        return reasons


def cargo_metadata(root: Path) -> dict:
    """The full `cargo metadata --locked` document, or a loud SystemExit."""
    try:
        result = subprocess.run(
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--manifest-path",
                str(root / "Cargo.toml"),
            ],
            cwd=root,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
        )
    except OSError as err:
        raise SystemExit(f"scoped gate: cargo metadata failed to start: {err}") from err
    if result.returncode != 0:
        raise SystemExit(
            f"scoped gate: cargo metadata failed (rc={result.returncode}) — the crate set is"
            f" unknown, not empty:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def parse_workspace(meta: dict) -> Workspace:
    """Member dirs → names, and reverse-dependent counts among members.

    Counts come from the resolve graph, so every dependency kind (normal,
    build, dev) counts once per dependent — a shared test-support crate is a
    hub as soon as four members' test suites lean on it.
    """
    root = Path(meta["workspace_root"])
    members = set(meta["workspace_members"])
    name_of = {pkg["id"]: pkg["name"] for pkg in meta["packages"]}
    crate_of_dir = {
        Path(pkg["manifest_path"]).parent.relative_to(root).as_posix(): pkg["name"]
        for pkg in meta["packages"]
        if pkg["id"] in members
    }
    build_scripts = frozenset(
        Path(target["src_path"]).relative_to(root).as_posix()
        for pkg in meta["packages"]
        if pkg["id"] in members
        for target in pkg.get("targets", [])
        if "custom-build" in target.get("kind", [])
    )
    rdeps = {name_of[pkg_id]: 0 for pkg_id in members}
    for node in meta["resolve"]["nodes"]:
        if node["id"] not in members:
            continue
        for dep in node["deps"]:
            dep_name = name_of.get(dep["pkg"])
            if dep_name in rdeps:
                rdeps[dep_name] += 1
    return Workspace(crate_of_dir=crate_of_dir, rdeps=rdeps, build_scripts=build_scripts)


# ---------------------------------------------------------------------------
# Git plumbing
# ---------------------------------------------------------------------------


def is_ancestor(root: Path, sha: str) -> bool:
    """rc 1 is the answer "no", not a failure — hence not `_git.git`."""
    result = subprocess.run(
        ["git", "-C", str(root), "merge-base", "--is-ancestor", sha, "HEAD"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    return result.returncode == 0


# ---------------------------------------------------------------------------
# The verify mark
# ---------------------------------------------------------------------------


def read_mark(mark_file: Path) -> dict:
    """The mark as a dict; `{}` when missing, unparseable or not an object."""
    try:
        mark = json.loads(mark_file.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return mark if isinstance(mark, dict) else {}


def full_head_of(mark: dict) -> str | None:
    """The head of the last full mark this mark remembers, if any."""
    if mark.get("scope") == "full":
        return mark.get("head")
    return mark.get("full_head")


def worktree_id(repo: Path) -> str:
    """The realpath'd working tree of ``repo`` — the identity a mark is bound to.

    The mark home is the *project dir*, shared by every linked worktree under
    `.agents/worktrees/<slug>`, so a batch that branches seventeen of them from
    one HEAD has seventeen trees reading and writing one file. HEAD alone then
    stops discriminating and each certifies the others' unverified trees — the
    reader in `commit_gate.py` compares this instead (R17).
    """
    return str(Path(git(repo, "rev-parse", "--show-toplevel").strip()).resolve())


def _real_index_env() -> dict[str, str]:
    """The environment with `GIT_INDEX_FILE` dropped, so git reads the real index."""
    return {name: value for name, value in os.environ.items() if name != "GIT_INDEX_FILE"}


def _git_run(repo: Path, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    """`git -C repo <args>` against the real index; the caller judges the exit code."""
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        encoding="utf-8",
        env=_real_index_env() if env is None else env,
        check=False,
    )


def index_tree(repo: Path) -> str | None:
    """`git write-tree` of ``repo``'s REAL index, or None when git cannot write one.

    `GIT_INDEX_FILE` is dropped on purpose: inside a hook git points it at the
    index being committed, and the mark's `tree` is defined as the tree of the
    repository's own index at mark time (adr_test_speed_tiers.md C-TIER). None
    is the unmerged-entries case, and every reader treats it as "no tree".
    """
    result = _git_run(repo, "write-tree")
    return (result.stdout.strip() or None) if result.returncode == 0 else None


def worktree_tree(repo: Path) -> str | None:
    """The tree `git add -A` would stage now: the working tree a run tested.

    Written into a throwaway index, never the real one. The run log records it
    beside the mark's `tree` because the ordinary flow is "verify, then stage,
    then commit": the index at mark time is then the parent's tree, and only
    this one equals the tip that is later merged.
    """
    git_dir = _git_run(repo, "rev-parse", "--absolute-git-dir")
    if git_dir.returncode != 0:
        return None
    real_index = Path(git_dir.stdout.strip()) / "index"
    with tempfile.TemporaryDirectory(prefix="ocx-gate-") as tmp:
        scratch = Path(tmp) / "index"
        if real_index.is_file():
            shutil.copyfile(real_index, scratch)
        env = {**_real_index_env(), "GIT_INDEX_FILE": str(scratch)}
        if _git_run(repo, "add", "-A", env=env).returncode != 0:
            return None
        result = _git_run(repo, "write-tree", env=env)
    return (result.stdout.strip() or None) if result.returncode == 0 else None


def merge_in_progress(repo: Path) -> bool:
    """True while ``repo``'s per-worktree git dir holds `MERGE_HEAD`."""
    return (Path(git(repo, "rev-parse", "--absolute-git-dir").strip()) / "MERGE_HEAD").exists()


def unstaged_or_untracked(repo: Path) -> list[str]:
    """Paths where the working tree differs from the index (AM-8), sorted."""
    listed = ""
    for args in (("diff", "--name-only"), ("ls-files", "--others", "--exclude-standard")):
        result = _git_run(repo, *args)
        if result.returncode != 0:
            raise SystemExit(f"git {' '.join(args)} failed (rc={result.returncode}):\n{result.stderr}")
        listed += result.stdout
    return sorted({line for line in listed.splitlines() if line})


def write_mark(
    mark_file: Path,
    scope: str,
    crates: list[str],
    head: str,
    toplevel: str,
    tree: str | None = None,
    nocache: bool = False,
) -> dict:
    """Write the mark; a scoped one carries the previous full head forward.

    `nocache` records a run that re-executed the acceptance suite
    (`task verify NOCACHE=1`) — the only full mark a `release:` commit takes.
    """
    mark = {
        "timestamp": int(time.time()),
        "head": head,
        "scope": scope,
        "crates": sorted(set(crates)),
        "toplevel": toplevel,
        "tree": tree,
        "nocache": nocache,
    }
    if scope != "full":
        full_head = full_head_of(read_mark(mark_file))
        if full_head:
            mark["full_head"] = full_head
    mark_file.parent.mkdir(parents=True, exist_ok=True)
    mark_file.write_text(json.dumps(mark, sort_keys=True) + "\n", encoding="utf-8")
    return mark


def _dirty_merge_refusal(repo: Path) -> str | None:
    """Why the working tree of an in-progress merge is not the tree to build, or None."""
    if not merge_in_progress(repo):
        return None
    paths = unstaged_or_untracked(repo)
    if paths:
        return (
            "scoped_gate: --mark: a merge is in progress and the working tree differs from the"
            f" index at {len(paths)} path(s): {', '.join(paths)} — stage or remove them, then re-run"
            " `task verify` (the merge commit is admitted only by a full mark of the tree it"
            " commits, so the tree built must be the tree staged)"
        )
    if index_tree(repo) is None:
        return (
            "scoped_gate: --mark: a merge is in progress and `git write-tree` fails (unmerged"
            " paths?) — resolve and stage every path, then re-run `task verify`"
        )
    return None


def start_snapshot(repo: Path) -> Path:
    """Where `--mark-precheck` records the merged index tree `task verify` started on.

    The per-worktree git dir: never in the working tree (it would be the
    untracked file AM-8 refuses) and never shared with a sibling worktree.
    """
    return Path(git(repo, "rev-parse", "--absolute-git-dir").strip()) / VERIFY_START


def mark_precheck(repo: Path) -> str | None:
    """`--mark-precheck`, `task verify`'s first step: AM-8 up front, and the start tree.

    The refusal `--mark` would give after the whole suite is given before it.
    Whenever the working tree equals the index — merging or not — the index
    tree is snapshot: it is the only evidence `--mark` accepts that the run
    built the tree it records (`proven_tree`). A dirty tree outside a merge
    is not refused; it just earns no snapshot, so its mark carries no tree.
    """
    snapshot = start_snapshot(repo)
    snapshot.unlink(missing_ok=True)
    refusal = _dirty_merge_refusal(repo)
    if refusal:
        return refusal.replace("--mark:", "--mark-precheck:", 1)
    tree = index_tree(repo)
    if tree and not unstaged_or_untracked(repo):
        snapshot.write_text(f"{tree}\n", encoding="utf-8")
    return None


def proven_tree(repo: Path) -> str | None:
    """The mark's `tree`: the index tree, only when this run proved it built it.

    Proof is index == working tree at the run's start (the precheck snapshot)
    and again now, with the index unchanged in between. Anything else is
    None, which the merge-commit clause refuses: a run over a working tree
    that differs from its index (e.g. `wp`'s tree staged over an untouched
    checkout) would otherwise certify a tree a later merge happens to stage.
    """
    try:
        started = start_snapshot(repo).read_text(encoding="utf-8").strip()
    except FileNotFoundError:
        return None
    now = index_tree(repo)
    if not now or now != started or unstaged_or_untracked(repo):
        return None
    return now


def mark_refusal(repo: Path) -> str | None:
    """Why `--mark` must not write here, or None (C-017, AM-8).

    Only while a merge is in progress: the merge commit's gate compares the
    mark's `tree` with the index, so a working tree that differs from the index
    would earn a mark for a tree nobody built — and so would an index restaged
    after `task verify` started, which `--mark-precheck`'s snapshot catches.
    """
    refusal = _dirty_merge_refusal(repo)
    if refusal or not merge_in_progress(repo):
        return refusal
    try:
        started = start_snapshot(repo).read_text(encoding="utf-8").strip()
    except FileNotFoundError:
        return (
            "scoped_gate: --mark: a merge is in progress and no `--mark-precheck` snapshot"
            " records the tree this verify started on — run `task verify`, which takes it first"
        )
    now = index_tree(repo)
    if started != now:
        return (
            f"scoped_gate: --mark: the index changed since `task verify` started ({started} ->"
            f" {now}) — the run built the first tree, not the one staged now; re-run `task verify`"
        )
    return None


# ---------------------------------------------------------------------------
# The scoped-run log and escape records (C-018, ADR C-ESC)
# ---------------------------------------------------------------------------

RUN_LOG = "scoped_runs.jsonl"
VERIFY_START = "ocx-gate-verify-start"
ESCAPES_LOG = "escapes.jsonl"
ESCAPE_MARKER = "gate_escape"


def gate_log_dir(repo: Path) -> Path:
    """`$(git rev-parse --git-common-dir)/ocx-gate` — shared by every worktree."""
    common = git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir").strip()
    return Path(common) / "ocx-gate"


def append_jsonl(path: Path, record: dict, unique_on: tuple[str, ...] = ()) -> bool:
    """Append one JSON line under an exclusive `fcntl.flock`; never truncates.

    With ``unique_on``, a line already holding the same values for those keys
    is read under the same lock and the append is skipped (returns False).
    """
    # Imported here, not at the top: `fcntl` is POSIX-only and commit_gate.py
    # imports this module on every commit, on every platform.
    import fcntl

    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a+", encoding="utf-8") as fh:
        fcntl.flock(fh, fcntl.LOCK_EX)  # released when the file closes
        if unique_on:
            fh.seek(0)
            key = tuple(record.get(name) for name in unique_on)
            if any(tuple(seen.get(name) for name in unique_on) == key for seen in _jsonl_records(fh)):
                return False
        # O_APPEND: the write lands at the end whatever the read above left.
        fh.write(json.dumps(record, sort_keys=True) + "\n")
    return True


def log_scoped_run(repo: Path, globs: list[str]) -> dict:
    """Append `{ts, worktree, tree, head, acceptance_globs}` to the run log."""
    record = {
        "ts": int(time.time()),
        "worktree": worktree_id(repo),
        "tree": index_tree(repo),
        "worktree_tree": worktree_tree(repo),
        "head": git(repo, "rev-parse", "HEAD").strip(),
        "acceptance_globs": sorted(set(globs)),
    }
    append_jsonl(gate_log_dir(repo) / RUN_LOG, record)
    return record


def log_run(repo: Path) -> None:
    """`--mark scoped --log-run`: log what verify:scoped selected; never fails the mark.

    The selection is recomputed from `make_plan` at mark time: the modules
    `test:scoped` ran (`acceptance_globs`) plus the edited ones `test:parallel`
    ran (`routes["tests"]`), so an edited-and-run module is not an escape. An
    escalated plan logs nothing — that run was the full verify, not a scoped one.
    """
    try:
        plan = make_plan(repo, mark_file())
        if plan.decision == "escalate":
            print("scoped_gate: --log-run: the plan escalates, so this is not a scoped run — not logged", file=sys.stderr)
            return
        log_scoped_run(repo, plan.acceptance_globs + plan.routes["tests"])
    except (SystemExit, OSError, ValueError, ImportError) as problem:
        # ImportError: no `fcntl` off POSIX — the mark above is written already.
        print(f"scoped_gate: --log-run: warning: the scoped run was not logged: {problem}", file=sys.stderr)


def failing_modules(junit_dir: Path, since: float | None = None) -> list[str]:
    """Modules whose `<junit_dir>/<module>/junit.xml` holds a failure or error.

    With ``since`` (epoch seconds), a report older than it is ignored: bazel
    leaves a target's last `test.xml` behind when an aborted invocation never
    ran it, and that stale failure is not this run's.
    """
    failing = []
    for report in sorted(junit_dir.glob("*/junit.xml")):
        if since is not None and report.stat().st_mtime < since:
            continue
        cases = ET.parse(report).getroot().iter("testcase")
        if any(case.find("failure") is not None or case.find("error") is not None for case in cases):
            failing.append(report.parent.name)
    return failing


def record_escapes(repo: Path, junit_dir: Path, since: float | None = None) -> int:
    """`--record-escapes`: attribute each failing module of a T2 run (C-018).

    Prints `escape: <module>` (and appends to escapes.jsonl and writes
    `<junit_dir>/gate_escape`) when at least one scoped run recorded the
    `MERGE_HEAD^{tree}` and none selected the module; `unattributed: <module>`
    when no record matches or no merge is in progress. Returns 0; a failure of
    its own is the caller's warning, never T2's status.
    """
    modules = failing_modules(junit_dir, since)
    if not modules:
        return 0
    if not merge_in_progress(repo):
        for module in modules:
            print(f"unattributed: {module}")
        print("scoped_gate: no merge in progress, so no scoped run can be attributed", file=sys.stderr)
        return 0
    merge_head_tree = git(repo, "rev-parse", "MERGE_HEAD^{tree}").strip()
    log = gate_log_dir(repo) / RUN_LOG
    try:
        with log.open(encoding="utf-8") as fh:
            runs = list(_jsonl_records(fh))
    except FileNotFoundError:
        runs = []
    # Either tree: `tree` when the WP staged before its scoped run,
    # `worktree_tree` when it verified first and staged after (worktree_tree).
    matching = [run for run in runs if merge_head_tree in (run.get("tree"), run.get("worktree_tree"))]
    scoped_globs = sorted({glob for run in matching for glob in run.get("acceptance_globs") or []})
    tree, head = index_tree(repo), git(repo, "rev-parse", "HEAD").strip()
    escaped = []
    for module in modules:
        if not matching:
            print(f"unattributed: {module}")
        elif not any(fnmatchcase(f"tests/{module}.py", glob) for glob in scoped_globs):
            append_jsonl(
                gate_log_dir(repo) / ESCAPES_LOG,
                {
                    "ts": int(time.time()),
                    "tree": tree,
                    "head": head,
                    "merge_head_tree": merge_head_tree,
                    "module": module,
                    "scoped_globs": scoped_globs,
                },
                unique_on=("tree", "merge_head_tree", "module"),
            )
            escaped.append(module)
            print(f"escape: {module}")
    if not matching:
        print(
            f"scoped_gate: no scoped run recorded the merged tip's tree {merge_head_tree}",
            file=sys.stderr,
        )
    if escaped:
        (junit_dir / ESCAPE_MARKER).write_text("\n".join(escaped) + "\n", encoding="utf-8")
    return 0


def _jsonl_records(lines) -> list[dict]:
    """The JSON-object lines of an open log; a torn or foreign line is skipped."""
    records = []
    for line in lines:
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if isinstance(record, dict):
            records.append(record)
    return records


def resolve_base(root: Path, mark_file: Path) -> tuple[str, str]:
    """(base sha, source) — source is "full-mark" or "merge-base(origin/main)"."""
    full_head = full_head_of(read_mark(mark_file))
    if full_head and is_ancestor(root, full_head):
        return full_head, "full-mark"
    return git(root, "merge-base", "origin/main", "HEAD").strip(), "merge-base(origin/main)"


# ---------------------------------------------------------------------------
# Changed paths and their classification
# ---------------------------------------------------------------------------


def changed_paths(root: Path, base: str) -> list[str]:
    """Paths changed between base and HEAD, plus the working tree (tracked and untracked)."""
    # `--no-renames`: a rename lists only its new path otherwise, so moving
    # `.github/workflows/x.yml` out of a [security] glob would hide the removal.
    listed = (
        git(root, "diff", "--name-only", "--no-renames", f"{base}...HEAD")
        + git(root, "diff", "--name-only", "--no-renames", "HEAD")
        + git(root, "ls-files", "--others", "--exclude-standard")
    )
    return sorted({line for line in listed.splitlines() if line})


@dataclass(slots=True)
class Plan:
    base: str
    base_source: str
    changed: list[str]
    crates: list[str] = field(default_factory=list)
    hubs: list[str] = field(default_factory=list)
    routes: dict[str, list[str]] = field(
        default_factory=lambda: {
            "claude": [],
            "workflows": [],
            "tests": [],
            "lint": [],
            "scripts": [],
            "manifests": [],
        }
    )
    workflow_test_cmd: str = ""
    escalate: list[str] = field(default_factory=list)
    decision: str = ""
    # C-014: what `test:scoped` runs — the sorted union of the modules the
    # changed command files' markers select and the scoped crates' [crates]
    # globs, each relative to test/. `reason` is the escalation, in one line.
    acceptance_globs: list[str] = field(default_factory=list)
    reason: str = ""

    def as_json(self) -> str:
        return json.dumps(asdict(self), indent=2, sort_keys=True, ensure_ascii=False)


def classify(
    plan: Plan,
    ws: Workspace,
    root: Path,
    rows: Rows,
    markers: dict[str, tuple[list[str], list[str]]],
) -> Plan:
    """Fill crates, hubs, routes, escalate, decision and acceptance_globs from plan.changed.

    C-014's order: a `[security]` glob escalates first, whatever else the path
    is (a manifest, a workflow, a command file); then a command file routes to
    the modules its marker selects unless `[verbs]` escalates it; then any
    other path under the CLI crate escalates (permit-list default); then the
    routes and crate logic below. A crate path still names its package in
    `crates` on an escalate, for information.
    """
    crates: set[str] = set()
    modules: set[str] = set()
    for path in plan.changed:
        parts = path.split("/")
        member = ws.crate_of_dir.get("/".join(parts[:2])) if parts[0] == "crates" and len(parts) > 2 else None
        glob = security_glob(path, rows)
        if glob:
            plan.escalate.append(f"{path}: [security] {glob} — full verify")
            if member:
                crates.add(member)
        elif path in ws.build_scripts:
            # Named by a manifest's `build =`, whatever the file is called.
            plan.escalate.append(f"{path}: a crate build script (cargo metadata custom-build) — full verify")
            if member:
                crates.add(member)
        elif path.startswith(COMMAND_DIR):
            crates.add(CLI_PACKAGE)
            key = path[len(COMMAND_DIR) :].removesuffix(".rs")
            pattern = verbs_pattern(key, rows)
            selected = selected_modules(key, markers)
            if pattern:
                plan.escalate.append(f"{path}: [verbs] {pattern} — a file several verbs share, full verify")
            elif not path.endswith(".rs") or not (root / path).is_file():
                plan.escalate.append(f"{path}: not an existing command file — full verify")
            elif not selected:
                plan.escalate.append(
                    f"{path}: no acceptance module carries a `command` marker for {key!r} — full verify"
                )
            else:
                modules.update(selected)
        elif path.startswith(CLI_DIR):
            # Its manifest and README too: the CLI crate's manifest decides the
            # shipped binary (features, `[[bin]]`), which no scoped subset certifies.
            crates.add(CLI_PACKAGE)
            plan.escalate.append(f"{path}: CLI crate path outside command/ — permit-list default, full verify")
        elif path == "CLAUDE.md" or parts[0] == ".agents" or path.startswith(CLAUDE_TESTS_READS):
            # `.agents/**` is swarm memory (hex.md, discussions): AI config
            # like `.claude/**`, and `claude:tests` is the cheapest gate
            # that reads it — an escalation to the full run bought nothing.
            plan.routes["claude"].append(path)
        elif parts[0] == ".github":
            plan.routes["workflows"].append(path)
        elif parts[0] == "scripts":
            # Gate tooling: its self-tests run (`task scripts:self-test`), and
            # the change still escalates — these scripts decide what every
            # other gate runs, so nothing less than the full verify (whose
            # `.verify:lint` runs `scripts:verify`) certifies an edit here.
            plan.routes["scripts"].append(path)
            plan.escalate.append(f"{path}: gate tooling — self-tests routed, full verify still required")
        elif parts[0] == "crates" and len(parts) == 3 and parts[2] in MANIFEST_FILES:
            # C-019 (2): a crate's manifest and README are workspace
            # structure, not that crate's code. Every guard over them —
            # `deps_direction` (C-009), `crate_map_toml_matches_rust_table`
            # (C-046), the internal-crates block, the may-depend-on rows,
            # `release_feature_set_excludes_testing_seams` — lives in one
            # test target, and resolving the path to its *own* package and
            # running `nextest -p <crate>` runs none of them: today's 17
            # shells have no reverse dependents, so a disallowed edge added
            # to one would pass a scoped gate green.
            #
            # The guards are not the whole route (D40). They compile
            # `ocx_test_support` and nothing else, while a manifest decides
            # what the WORKSPACE compiles — `[features]`, a dependency edge,
            # `default-features` — and `Cargo.lock` carries no feature data,
            # so such an edit never drags the lock in and never escalates.
            # `verify:scoped` therefore runs `cargo check --workspace
            # --all-targets --locked` on this route as well, and
            # `the_manifest_route_reaches_a_workspace_compile` pins that step.
            #
            # That target used to be a test of package `ocx`, which is why
            # this arm escalated to the full verify — reaching it meant
            # building the whole CLI. D34 moved it to
            # `crates/ocx_test_support/tests/workspace_structure.rs`, whose
            # only dependencies are syn/proc-macro2/quote plus four dev
            # crates, so `verify:scoped` can run it directly (measured: 23
            # tests, 6.3 s) and the route is now what certifies the path.
            # The command lives in that step, beside `ci:actionlint`, not in
            # a plan field — a field nothing consumes is dead state, and a
            # self-test comparing it to the constant it was assigned from is
            # a green that cannot tell itself from never having run.
            plan.routes["manifests"].append(path)
        elif parts[0] == "crates" and len(parts) > 2 and f"crates/{parts[1]}" in ws.crate_of_dir:
            crates.add(ws.crate_of_dir[f"crates/{parts[1]}"])
        elif (
            len(parts) == 3
            and parts[:2] == ["test", "tests"]
            and parts[2].startswith("test_")
            and parts[2].endswith(".py")
        ):
            if (root / path).is_file():
                plan.routes["tests"].append("/".join(parts[1:]))
            else:
                plan.escalate.append(f"{path}: deleted acceptance file — full verify")
        elif parts[:2] == ["test", "lint"] or path in LINT_TIER_FILES:
            # The lint tier runs uncached on every non-escalating arm, so its
            # own edit is certified by that run plus ruff over the suite.
            plan.routes["lint"].append(path)
        else:
            plan.escalate.append(f"{path}: no route — full verify")
    plan.crates = sorted(crates)
    plan.hubs = ws.hubs()
    globs: set[str] = set()
    for crate in plan.crates:
        for reason in ws.hub_reasons(crate):
            plan.escalate.append(f"{crate}: {reason} — full verify")
        if crate == CLI_PACKAGE:
            continue
        row = rows.crates.get(crate)
        if row is None:
            plan.escalate.append(f"{crate}: no [crates] row in test/scoped_rows.toml — full verify")
        elif row == "escalate":
            plan.escalate.append(f"{crate}: [crates] row reads escalate — full verify")
        else:
            globs.update(row)
    plan.acceptance_globs = sorted(modules | globs)
    plan.reason = "; ".join(plan.escalate)
    if plan.routes["workflows"]:
        plan.workflow_test_cmd = (
            f"uv run --directory {WORKFLOW_TEST.parent.as_posix()} pytest {WORKFLOW_TEST.name} -q"
        )
    if plan.escalate:
        plan.decision = "escalate"
    elif plan.crates:
        plan.decision = "scoped"
    else:
        plan.decision = "routed"
    return plan


def make_plan(root: Path, mark_file: Path) -> Plan:
    # The table first: a missing or malformed one must be the named failure,
    # before cargo or git can fail for a reason of their own.
    rows = load_rows(root / "test" / "scoped_rows.toml")
    ws = parse_workspace(cargo_metadata(root))
    base, source = resolve_base(root, mark_file)
    plan = Plan(base=base, base_source=source, changed=changed_paths(root, base))
    return classify(plan, ws, root, rows, module_markers(root / "test"))


def check_coverage(root: Path) -> int:
    """`--check-coverage`: print every finding, exit 1 on any."""
    findings = live_coverage(root)
    for finding in findings:
        print(f"test:rows:check: {finding}", file=sys.stderr)
    if findings:
        print(f"test:rows:check: {len(findings)} finding(s) — test/scoped_rows.toml and the markers do not cover the tree", file=sys.stderr)
        return 1
    markers = module_markers(root / "test")
    print(
        f"test:rows:check: OK — {len(markers)} acceptance modules and {len(command_keys(root))} command files"
        f" covered (floors {gate_module_floor(root / 'scripts' / 'bazel_gate_proofs.py')} / {COMMAND_FILES_FLOOR})"
    )
    return 0


# ---------------------------------------------------------------------------
# --self-test
# ---------------------------------------------------------------------------


def _fixture_metadata(root: Path) -> dict:
    """A `cargo metadata` document in cargo's shape, small enough to reason about.

    `ocx_store` has five reverse dependents (a hub by count, not by tier);
    `ocx_exit` has exactly HUB_RDEPS (the boundary: `>=` is the rule, and a
    `>` would let it through); `ocx_oci` has three (a hub by tier only);
    `ocx_shell` has two; `serde` is the external crate everything depends on
    and must never surface.
    """
    dirs = {
        "ocx_exit": "ocx_exit",
        "ocx": "ocx_cli",
        "ocx_schema": "ocx_schema",
        "ocx_setup": "ocx_setup",
        "ocx_oci": "ocx_oci",
        "ocx_util": "ocx_util",
        "ocx_store": "ocx_store",
        "ocx_index": "ocx_index",
        "ocx_package": "ocx_package",
        "ocx_shell": "ocx_shell",
        "ocx_project": "ocx_project",
        "ocx_test_support": "ocx_test_support",
    }
    deps = {
        "ocx": ["ocx_exit"],
        "ocx_schema": ["ocx_package"],
        "ocx_oci": ["ocx_util"],
        "ocx_store": ["ocx_oci", "ocx_util"],
        "ocx_index": ["ocx_store", "ocx_oci", "ocx_util"],
        "ocx_package": ["ocx_store", "ocx_index", "ocx_oci", "ocx_util"],
        "ocx_shell": ["ocx_store", "ocx_package", "ocx_util", "ocx_exit"],
        "ocx_project": ["ocx_store", "ocx_shell", "ocx_package", "ocx_util", "ocx_exit"],
        "ocx_setup": ["ocx_shell", "ocx_store", "ocx_util", "ocx_test_support", "ocx_exit"],
    }

    def pkg_id(name: str) -> str:
        return f"path+file://{root}/crates/{dirs.get(name, name)}#{name}@0.6.2"

    packages = [
        {
            "name": name,
            "id": pkg_id(name),
            "manifest_path": str(root / "crates" / d / "Cargo.toml"),
        }
        for name, d in dirs.items()
    ]
    packages.append(
        {"name": "serde", "id": "registry+serde@1.0.0", "manifest_path": "/reg/serde/Cargo.toml"}
    )
    nodes = [
        {
            "id": pkg_id(name),
            "deps": [{"name": d, "pkg": pkg_id(d)} for d in deps.get(name, [])]
            + [{"name": "serde", "pkg": "registry+serde@1.0.0"}],
        }
        for name in dirs
    ]
    nodes.append({"id": "registry+serde@1.0.0", "deps": []})
    return {
        "packages": packages,
        "workspace_members": [pkg_id(name) for name in dirs],
        "workspace_root": str(root),
        "resolve": {"nodes": nodes},
    }



# The routing fixture's table (C-012 shape): one row per fixture member but
# `ocx`, a [verbs] pattern, and a [security] list with a path of every kind.
_FIXTURE_ROWS = """\
[crates]
ocx_exit = ["tests/test_install.py"]
ocx_schema = ["tests/test_install.py"]
ocx_setup = ["tests/test_self_setup.py", "tests/test_session_path.py"]
ocx_oci = ["tests/test_install.py"]
ocx_util = ["tests/test_install.py"]
ocx_store = ["tests/test_install.py"]
ocx_index = ["tests/test_install.py"]
ocx_package = ["tests/test_install.py"]
ocx_shell = ["tests/test_install.py"]
ocx_project = ["tests/test_install.py"]
ocx_test_support = "escalate"

[verbs]
"*_common.rs" = "escalate"

[security]
escalate = [".github/workflows/**", "crates/ocx_oci/**", "crates/ocx_store/**",
            "crates/*/build.rs", "crates/ocx_cli/src/command/login.rs"]
"""
# Command-file keys the fixture tree holds (C-013 key = path under command/ without .rs).
_FIXTURE_COMMANDS = (
    "package_push",
    "package_push_mount",
    "index_common",
    "unmarked",
    "login",
    "self_group/activate",
    "index_list",
    "index_update",
)
# One module per C-013 marker edge; each binds pytestmark the way its name says.
_FIXTURE_MODULES = {
    "test_self_setup.py": "def test_x(): pass\n",
    "test_session_path.py": "def test_x(): pass\n",
    "test_pushes.py": (
        "import pytest\n\n"
        'pytestmark = pytest.mark.command("package_push*")\n'
    ),
    "test_after_skipif.py": (
        "import sys\n\nimport pytest\n\n"
        'pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="posix")\n'
        'pytestmark = [pytestmark, pytest.mark.command("login")]\n'
    ),
    "test_appended.py": (
        "import sys\n\nimport pytest\n\n"
        'pytestmark = [pytest.mark.skipif(sys.platform == "win32", reason="posix")]\n'
        'pytestmark.append(pytest.mark.command("self_group/activate"))\n'
    ),
    "test_multi.py": (
        "import pytest\n\n"
        'pytestmark = pytest.mark.command("index_list", "index_update")\n'
    ),
}


def _plan_for(
    paths: list[str],
    ws: Workspace,
    root: Path,
    rows: Rows | None = None,
    markers: dict[str, tuple[list[str], list[str]]] | None = None,
) -> Plan:
    rows = rows if rows is not None else load_rows(root / "test" / "scoped_rows.toml")
    markers = markers if markers is not None else module_markers(root / "test")
    return classify(Plan(base="base", base_source="fixture", changed=sorted(paths)), ws, root, rows, markers)


def _self_test_cases(tmp: Path) -> list[tuple[str, object]]:
    """(name, thunk) pairs; a thunk returns None when the rule holds, else the problem."""
    root = tmp / "repo"
    (root / "test" / "tests").mkdir(parents=True)
    (root / "test" / "tests" / "test_install.py").write_text("def test_x(): pass\n", encoding="utf-8")
    (root / "test" / "scoped_rows.toml").write_text(_FIXTURE_ROWS, encoding="utf-8")
    for name, text in _FIXTURE_MODULES.items():
        (root / "test" / "tests" / name).write_text(text, encoding="utf-8")
    for key in _FIXTURE_COMMANDS:
        command = root / COMMAND_DIR / f"{key}.rs"
        command.parent.mkdir(parents=True, exist_ok=True)
        command.write_text("// fixture\n", encoding="utf-8")
    ws = parse_workspace(_fixture_metadata(root))

    def expect(cond: bool, problem: str) -> str | None:
        return None if cond else problem

    def crate_map() -> str | None:
        return expect(
            ws.crate_of_dir.get("crates/ocx_cli") == "ocx"
            and ws.crate_of_dir.get("crates/ocx_setup") == "ocx_setup"
            and len(ws.crate_of_dir) == 12
            and "serde" not in ws.rdeps,
            f"crate map wrong: {ws.crate_of_dir}",
        )

    def rdeps() -> str | None:
        return expect(
            ws.rdeps["ocx_store"] == 5
            and ws.rdeps["ocx_exit"] == HUB_RDEPS
            and ws.rdeps["ocx_shell"] == 2
            and ws.rdeps["ocx_setup"] == 0,
            f"reverse-dependent counts wrong: {ws.rdeps}",
        )

    def hubs() -> str | None:
        got = set(ws.hubs())
        return expect(
            "ocx_store" in got
            and "ocx_exit" in got
            and "ocx_oci" in got
            and "ocx_util" in got
            and "ocx_shell" not in got
            and "ocx_setup" not in got,
            f"hub set wrong: {sorted(got)}",
        )

    def routes() -> str | None:
        plan = _plan_for(
            [
                ".agents/memory/hex.md",
                ".claude/rules/x.md",
                "CLAUDE.md",
                ".github/ISSUE_TEMPLATE/bug.yml",
                "test/tests/test_install.py",
            ],
            ws,
            root,
        )
        return expect(
            plan.routes["claude"] == [".agents/memory/hex.md", ".claude/rules/x.md", "CLAUDE.md"]
            and plan.routes["workflows"] == [".github/ISSUE_TEMPLATE/bug.yml"]
            and plan.routes["tests"] == ["tests/test_install.py"]
            and plan.routes["scripts"] == []
            and plan.workflow_test_cmd == "uv run --directory .claude/tests pytest test_workflows.py -q"
            and plan.escalate == []
            and plan.decision == "routed",
            f"routing wrong: {plan.as_json()}",
        )

    def nested_taskfile_escalates() -> str | None:
        # `.claude/taskfile.yml` is not in the permit list, so it escalates
        # like every other taskfile instead of riding the `.claude/` route.
        plan = _plan_for([".claude/taskfile.yml", "website/sbom.taskfile.yml", ".claude/rules/x.md"], ws, root)
        return expect(
            plan.decision == "escalate"
            and plan.routes["claude"] == [".claude/rules/x.md"]
            and [r.split(":", 1)[0] for r in plan.escalate] == [".claude/taskfile.yml", "website/sbom.taskfile.yml"],
            f"every taskfile must escalate by name: {plan.as_json()}",
        )

    def unlisted_claude_subtree_escalates() -> str | None:
        # The permit list, not a forbid list: a .claude/ path nothing tests is
        # loud, and every listed subtree still routes.
        plan = _plan_for([".claude/scripts/review_surface.py"], ws, root)
        listed = _plan_for([f"{p}x.md" if p.endswith("/") else p for p in CLAUDE_TESTS_READS], ws, root)
        return expect(
            plan.decision == "escalate"
            and plan.routes["claude"] == []
            and listed.decision == "routed"
            and len(listed.routes["claude"]) == len(CLAUDE_TESTS_READS),
            f"unlisted .claude/ path must escalate, listed ones route: {plan.as_json()} {listed.as_json()}",
        )

    def scripts_route_and_escalate() -> str | None:
        plan = _plan_for(["scripts/scoped_gate.py"], ws, root)
        return expect(
            plan.routes["scripts"] == ["scripts/scoped_gate.py"]
            and plan.decision == "escalate"
            and any(r.startswith("scripts/scoped_gate.py:") and "self-tests routed" in r for r in plan.escalate),
            f"scripts/** must be routed to the self-tests AND escalate: {plan.as_json()}",
        )

    def manifest_routes_without_escalating() -> str | None:
        # `ocx_setup` is the fixture's zero-reverse-dependent shell, the shape
        # all 17 shells have today: before C-019 (2) named the manifest, this
        # resolved to the package and decided `scoped`, and the scoped tier's
        # `nextest -p ocx_setup` runs none of the workspace guards. Since D34
        # the guards are a test target of `ocx_test_support`, so the route
        # certifies the path on its own and the full verify is not spent.
        for path in ("crates/ocx_setup/Cargo.toml", "crates/ocx_setup/README.md"):
            plan = _plan_for([path], ws, root)
            problem = expect(
                plan.routes["manifests"] == [path]
                and plan.crates == []
                and plan.decision == "routed"
                and plan.escalate == [],
                f"{path} must route to the workspace-structure guards and not escalate: {plan.as_json()}",
            )
            if problem:
                return problem
        return None

    def crate_source_is_still_scoped_beside_a_manifest() -> str | None:
        # The manifest arm must not swallow the crate's own code: a source
        # edit still scopes with no manifest route, and a mixed change set
        # carries both — the crate's per-crate steps and the guards.
        source = _plan_for(["crates/ocx_setup/src/lib.rs"], ws, root)
        mixed = _plan_for(["crates/ocx_setup/src/lib.rs", "crates/ocx_setup/Cargo.toml"], ws, root)
        return expect(
            source.decision == "scoped"
            and source.routes["manifests"] == []
            and mixed.decision == "scoped"
            and mixed.crates == ["ocx_setup"]
            and mixed.routes["manifests"] == ["crates/ocx_setup/Cargo.toml"],
            f"a source edit must stay scoped and a mixed set carry both: {source.as_json()} {mixed.as_json()}",
        )

    def a_manifest_beside_an_escalating_path_does_not_run_the_guards_twice() -> str | None:
        # The taskfile step is guarded by `decision != escalate`, so the route
        # may stay populated on an escalate — the full verify runs the guards.
        # What must never happen is the escalation being *caused* by the
        # manifest: the reason names the other path and only the other path.
        plan = _plan_for(["crates/ocx_setup/Cargo.toml", "Cargo.lock"], ws, root)
        return expect(
            plan.decision == "escalate"
            and plan.routes["manifests"] == ["crates/ocx_setup/Cargo.toml"]
            and [reason.split(":", 1)[0] for reason in plan.escalate] == ["Cargo.lock"],
            f"only the unrouted path may escalate a manifest-plus-lockfile set: {plan.as_json()}",
        )

    def unrouted_paths_escalate() -> str | None:
        paths = [
            "test/tests/test_missing.py",
            "test/tests/conftest.py",
            "test/tests/fake_forge.py",
            "test/conftest.py",
            "test/src/x.py",
            "taskfile.yml",
            "Cargo.lock",
            "external/rust-oci-client/src/lib.rs",
            "crates/NEXTEST_FLOOR",
        ]
        plan = _plan_for(paths, ws, root)
        named = [reason.split(":", 1)[0] for reason in plan.escalate]
        return expect(
            plan.decision == "escalate" and named == sorted(paths) and plan.crates == [],
            f"unrouted paths must each escalate by name: {plan.as_json()}",
        )

    def non_hub_crate_is_scoped() -> str | None:
        plan = _plan_for(["crates/ocx_setup/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "scoped" and plan.crates == ["ocx_setup"] and plan.escalate == [],
            f"ocx_setup must be scoped: {plan.as_json()}",
        )

    def ecosystem_crate_escalates() -> str | None:
        plan = _plan_for(["crates/ocx_oci/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate" and any("ecosystem" in r for r in plan.escalate),
            f"ocx_oci must escalate as ecosystem tier: {plan.as_json()}",
        )

    def hub_by_count_escalates() -> str | None:
        plan = _plan_for(["crates/ocx_store/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate" and any("5 reverse dependents" in r for r in plan.escalate),
            f"ocx_store must escalate by reverse-dependent count: {plan.as_json()}",
        )

    def hub_at_the_boundary_escalates() -> str | None:
        # Exactly HUB_RDEPS dependents is a hub: the one fixture that tells
        # `>=` from `>`.
        plan = _plan_for(["crates/ocx_exit/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate"
            and any(f"{HUB_RDEPS} reverse dependents (hub at {HUB_RDEPS})" in r for r in plan.escalate),
            f"ocx_exit ({HUB_RDEPS} dependents) must escalate as a hub: {plan.as_json()}",
        )

    def table_escalate_row_escalates() -> str | None:
        # Was `ocx_lib`, whose row left with the crate at WP-37.
        plan = _plan_for(["crates/ocx_test_support/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate"
            and any("escalate" in r and "ocx_test_support" in r for r in plan.escalate),
            f"ocx_test_support must escalate (test:scoped row): {plan.as_json()}",
        )

    def marked_verb_is_scoped() -> str | None:
        # S-003: the key is fnmatch'd, so `package_push*` reaches both files.
        plan = _plan_for([f"{COMMAND_DIR}package_push.rs"], ws, root)
        mount = _plan_for([f"{COMMAND_DIR}package_push_mount.rs"], ws, root)
        return expect(
            plan.decision == "scoped"
            and plan.crates == [CLI_PACKAGE]
            and plan.escalate == []
            and plan.acceptance_globs == ["tests/test_pushes.py"]
            and mount.acceptance_globs == ["tests/test_pushes.py"],
            f"a marked verb must scope to its module: {plan.as_json()} {mount.as_json()}",
        )

    def common_file_escalates() -> str | None:
        # S-005: shared by several verbs, so [verbs] escalates it by pattern.
        plan = _plan_for([f"{COMMAND_DIR}index_common.rs"], ws, root)
        return expect(
            plan.decision == "escalate"
            and any("[verbs]" in r and "*_common.rs" in r for r in plan.escalate)
            and "index_common.rs" in plan.reason,
            f"*_common.rs must escalate by its [verbs] pattern: {plan.as_json()}",
        )

    def unmarked_command_escalates() -> str | None:
        # S-007's routing half: no marker, no route — and the reason names the file.
        plan = _plan_for([f"{COMMAND_DIR}unmarked.rs"], ws, root)
        return expect(
            plan.decision == "escalate"
            and [r.split(":", 1)[0] for r in plan.escalate] == [f"{COMMAND_DIR}unmarked.rs"]
            and "unmarked" in plan.reason,
            f"an unmarked command file must escalate by name: {plan.as_json()}",
        )

    def cli_non_command_escalates() -> str | None:
        # S-006: the permit-list default for the rest of the CLI crate.
        paths = [
            f"{CLI_DIR}src/app/context.rs", f"{CLI_DIR}src/command.rs", f"{CLI_DIR}tests/cli.rs",
            f"{CLI_DIR}Cargo.toml", f"{CLI_DIR}README.md",
        ]
        plans = [_plan_for([path], ws, root) for path in paths]
        return expect(
            all(p.decision == "escalate" and p.escalate[0].startswith(p.changed[0] + ":") for p in plans),
            f"CLI code outside command/ must escalate: {[p.as_json() for p in plans]}",
        )

    def build_script_escalates() -> str | None:
        plan = _plan_for([f"{CLI_DIR}build.rs"], ws, root)
        return expect(
            plan.decision == "escalate" and any("crates/*/build.rs" in r for r in plan.escalate),
            f"a crate build script must escalate by its [security] glob: {plan.as_json()}",
        )

    def every_security_glob_escalates() -> str | None:
        # One red per LIVE [security] glob (C-014): a path built to match the
        # glob must escalate with that glob — and no earlier one — named. A
        # glob another one shadows is dead text, and reds here too.
        live = load_rows(ROWS_FILE)
        if len(live.security) < 13:
            return f"read {len(live.security)} [security] globs off {ROWS_FILE}, fewer than the ADR list"
        for glob in live.security:
            path = glob.replace("**", "x/y.rs").replace("*", "x")
            first = next((g for g in live.security if fnmatchcase(path, g)), None)
            if first != glob:
                return f"{path!r} (built from {glob!r}) is matched first by {first!r}: the glob is shadowed"
            plan = _plan_for([path], ws, root, rows=live)
            named = [r for r in plan.escalate if r.startswith(f"{path}:") and f"[security] {glob}" in r]
            if plan.decision != "escalate" or not named or glob not in plan.reason:
                return f"{path!r} must escalate naming [security] {glob!r}: {plan.as_json()}"
        return None

    def self_update_escalates() -> str | None:
        # `self_update` swaps the ocx binary in place; its home is a crate
        # row whose globs select no self-update test, so only [security] routes it.
        path = "crates/ocx_package_manager/src/tasks/update_check.rs"
        plan = _plan_for([path], ws, root, rows=load_rows(ROWS_FILE))
        return expect(
            plan.decision == "escalate" and f"{path}: [security] {path}" in plan.escalate[0],
            f"{path} must escalate by its own [security] glob: {plan.as_json()}",
        )

    def ocx_setup_is_scoped() -> str | None:
        # S-001: the control — a non-hub crate scopes, and its [crates] globs
        # are what test:scoped runs.
        plan = _plan_for(["crates/ocx_setup/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "scoped"
            and plan.crates == ["ocx_setup"]
            and plan.acceptance_globs == ["tests/test_self_setup.py", "tests/test_session_path.py"]
            and plan.reason == "",
            f"an ocx_setup edit must be scoped over its row: {plan.as_json()}",
        )

    def _plan_exit(project: Path) -> subprocess.CompletedProcess[str]:
        """`--plan` from a throwaway copy of the script, in its own project."""
        (project / "scripts").mkdir(parents=True, exist_ok=True)
        for name in ("scoped_gate.py", "_git.py"):
            (project / "scripts" / name).write_bytes((REPO_ROOT / "scripts" / name).read_bytes())
        return subprocess.run(
            [sys.executable, str(project / "scripts" / "scoped_gate.py"), "--plan"],
            capture_output=True, text=True, encoding="utf-8", cwd=str(project), check=False,
        )

    def missing_rows_exit_1() -> str | None:
        # Never a silent escalate: rc 1, the file named, and no plan printed.
        run = _plan_exit(tmp / "no-rows")
        return expect(
            run.returncode == 1 and "scoped_rows.toml" in run.stderr and '"decision"' not in run.stdout,
            f"a missing table must exit 1 naming it: rc={run.returncode} out={run.stdout!r} err={run.stderr!r}",
        )

    def malformed_rows_exit_1() -> str | None:
        bad_syntax = tmp / "bad-syntax"
        (bad_syntax / "test").mkdir(parents=True)
        (bad_syntax / "test" / "scoped_rows.toml").write_text("[crates\nocx = 1\n", encoding="utf-8")
        run = _plan_exit(bad_syntax)
        if not (run.returncode == 1 and "malformed" in run.stderr and '"decision"' not in run.stdout):
            return f"a TOML syntax error must exit 1: rc={run.returncode} err={run.stderr!r}"
        # Schema, not syntax: each of these parses and is still not a table.
        shapes = {
            "missing [security]": _FIXTURE_ROWS.split("[security]")[0],
            "a [verbs] value other than escalate": _FIXTURE_ROWS.replace('= "escalate"\n\n[security]', '= "scoped"\n\n[security]'),
            "an empty glob list": _FIXTURE_ROWS.replace('ocx_exit = ["tests/test_install.py"]', "ocx_exit = []"),
            "a non-string glob": _FIXTURE_ROWS.replace('ocx_exit = ["tests/test_install.py"]', "ocx_exit = [1]"),
            "an unknown section": _FIXTURE_ROWS + "\n[extra]\nx = 1\n",
            "a string row other than escalate": _FIXTURE_ROWS.replace('ocx_exit = ["tests/test_install.py"]', 'ocx_exit = "tests/test_install.py"'),
            "[security] without escalate": _FIXTURE_ROWS.replace("escalate = [\".github", "globs = [\".github"),
            "an ocx row": _FIXTURE_ROWS.replace("[verbs]", 'ocx = ["tests/test_install.py"]\n\n[verbs]'),
        }
        for label, text in shapes.items():
            if text == _FIXTURE_ROWS:
                return f"the {label!r} mutation did not land"
            path = tmp / "shape.toml"
            path.write_text(text, encoding="utf-8")
            try:
                load_rows(path)
            except SystemExit as stop:
                if "malformed" not in str(stop):
                    return f"{label}: wrong failure text {stop}"
                continue
            return f"{label}: load_rows accepted a malformed table"
        return None

    def marker_append_is_read() -> str | None:
        keys, problems = read_markers((root / "test" / "tests" / "test_appended.py").read_text(encoding="utf-8"))
        plan = _plan_for([f"{COMMAND_DIR}self_group/activate.rs"], ws, root)
        return expect(
            keys == ["self_group/activate"] and problems == []
            and plan.acceptance_globs == ["tests/test_appended.py"],
            f"pytestmark.append(command(...)) must be read: {keys} {problems} {plan.as_json()}",
        )

    def marker_after_skipif_is_read() -> str | None:
        keys, problems = read_markers((root / "test" / "tests" / "test_after_skipif.py").read_text(encoding="utf-8"))
        return expect(
            keys == ["login"] and problems == [],
            f"[pytestmark, command('login')] after a skipif must read as login only: {keys} {problems}",
        )

    def multi_key_marker_selects_each() -> str | None:
        plans = [_plan_for([f"{COMMAND_DIR}{key}.rs"], ws, root) for key in ("index_list", "index_update")]
        return expect(
            all(p.decision == "scoped" and p.acceptance_globs == ["tests/test_multi.py"] for p in plans),
            f"a multi-key marker must select on each key: {[p.as_json() for p in plans]}",
        )

    def non_literal_marker_is_unreadable() -> str | None:
        keys, problems = read_markers('import pytest\n\nKEY = "x"\npytestmark = pytest.mark.command(KEY)\n')
        kw_keys, kw_problems = read_markers('import pytest\n\npytestmark = pytest.mark.command(key="x")\n')
        return expect(
            keys == [] and len(problems) == 1 and kw_keys == [] and len(kw_problems) == 1,
            f"a non-literal or keyword command mark must be unreadable: {problems} {kw_problems}",
        )

    def stray_command_mark_is_unreadable() -> str | None:
        # pytest applies some of these (a decorator, a bare `mark`); the gate
        # reads one spelling at module scope, so each is a problem rather than
        # a key silently credited or silently lost.
        sources = {
            "decorator": 'import pytest\n\n\n@pytest.mark.command("x")\ndef test_x():\n    pass\n',
            "dropped": 'import pytest\n\npytestmark = pytest.mark.command("x")\npytestmark = pytest.mark.smoke\n',
            "bare mark": 'from pytest import mark\n\npytestmark = mark.command("x")\n',
            "alias": 'import pytest as pt\n\npytestmark = pt.mark.command("x")\n',
            "nested": 'import sys\n\nimport pytest\n\nif sys:\n    pytestmark = pytest.mark.command("x")\n',
            "augmented": 'import pytest\n\npytestmark = []\npytestmark += [pytest.mark.command("x")]\n',
            "no key": 'import pytest\n\npytestmark = pytest.mark.command()\n',
        }
        for label, source in sources.items():
            keys, problems = read_markers(source)
            if keys or len(problems) != 1:
                return f"{label}: must read no key and one problem, got {keys} {problems}"
        return None

    def lint_tier_routes() -> str | None:
        # WP-05's deferral: the lint tier runs on every non-escalating arm,
        # so a lint-tier edit is certified by it rather than by the full run.
        paths = ["test/lint/test_x_structure.py", "test/lint/conftest.py", *LINT_TIER_FILES]
        plan = _plan_for(paths, ws, root)
        return expect(
            plan.decision == "routed" and plan.routes["lint"] == sorted(paths) and plan.escalate == [],
            f"test/lint/** and {LINT_TIER_FILES} must route to the lint tier: {plan.as_json()}",
        )

    # C-015 — the coverage guard, over its own small tree so each invariant
    # can be broken alone. Command floor 4, module floor 3 for this fixture;
    # the live check uses COMMAND_FILES_FLOOR and ACCEPTANCE_MODULE_TARGETS.
    cov = tmp / "coverage"
    (cov / "test" / "tests").mkdir(parents=True)
    for key in ("package_push", "index_common", "login", "self_group/setup"):
        command = cov / COMMAND_DIR / f"{key}.rs"
        command.parent.mkdir(parents=True, exist_ok=True)
        command.write_text("// fixture\n", encoding="utf-8")
    # Every TABLE_ESCALATES row, so the fixture follows the constant it is
    # cross-checked against instead of restating it.
    cov_escalates: dict[str, list[str] | str] = dict.fromkeys(sorted(TABLE_ESCALATES), "escalate")
    cov_rows = Rows(
        crates={"ocx_setup": ["tests/test_self_*.py"], **cov_escalates},
        verbs={"*_common.rs": "escalate"},
        security=list(REQUIRED_SECURITY),
    )
    cov_members = {CLI_PACKAGE, "ocx_setup", *TABLE_ESCALATES}
    cov_markers = {
        "tests/test_self_setup.py": (["self_group/*"], []),
        "tests/test_push.py": (["package_push"], []),
        "tests/test_other.py": (["package_push*"], []),
    }
    cov_hex = '- role: reviewer:security\n  when: "{.github/workflows/**}"\n'
    # One tracked path per REQUIRED_SECURITY glob, built the way the per-glob
    # routing case builds them, so the fixture's table guards something.
    cov_tracked = [glob.replace("**", "x/y.rs").replace("*", "x") for glob in REQUIRED_SECURITY]

    def coverage(**change: object) -> list[str]:
        args: dict[str, object] = {
            "rows": cov_rows,
            "markers": cov_markers,
            "members": cov_members,
            "module_floor": 3,
            "command_floor": 4,
            "hex_text": cov_hex,
            "tracked": list(cov_tracked),
        }
        args.update(change)
        return coverage_findings(cov, **args)  # type: ignore[arg-type]

    def one_red(findings: list[str], needle: str) -> str | None:
        return expect(
            len(findings) == 1 and needle in findings[0],
            f"expected exactly one finding naming {needle!r}, got {findings}",
        )

    def coverage_fixture_green() -> str | None:
        found = coverage()
        return expect(found == [], f"the coverage fixture must be green: {found}")

    def coverage_red_module() -> str | None:
        # S-008: a module no glob matches and no marker names.
        return one_red(coverage(markers={**cov_markers, "tests/test_foo.py": ([], [])}), "tests/test_foo.py")

    def coverage_red_command() -> str | None:
        # S-007: a command file no marker, [verbs] entry or [security] glob names.
        (cov / COMMAND_DIR / "foo.rs").write_text("// fixture\n", encoding="utf-8")
        try:
            return one_red(coverage(command_floor=5), f"{COMMAND_DIR}foo.rs")
        finally:
            (cov / COMMAND_DIR / "foo.rs").unlink()

    def coverage_red_security() -> str | None:
        hex_text = '- role: reviewer:security\n  when: "{.github/workflows/**,crates/ocx_announce/**}"\n'
        return one_red(coverage(hex_text=hex_text), "crates/ocx_announce/**")

    def coverage_red_unparseable_when() -> str | None:
        for hex_text in (
            '- role: reviewer:security\n  when: "{a,{b}}"\n',
            '- role: reviewer:security\n  when: "{}"\n',
            "- role: reviewer:quality\n",
        ):
            found = coverage(hex_text=hex_text)
            if len(found) != 1 or "when:" not in found[0]:
                return f"an unparseable reviewer:security when: must red once, got {found} for {hex_text!r}"
        return None

    def coverage_red_module_floor() -> str | None:
        # The floor is read independently of the globs: three modules read
        # against a floor of four reds even though every glob still matches.
        return one_red(coverage(module_floor=4), "floor")

    def coverage_red_command_floor() -> str | None:
        return one_red(coverage(command_floor=5), "floor")

    def coverage_red_unreadable() -> str | None:
        markers = {**cov_markers, "tests/test_bad.py": ([], ["line 3: not a literal"])}
        return one_red(coverage(markers=markers), "tests/test_bad.py")

    def coverage_red_table_drift() -> str | None:
        dead = Rows({**cov_rows.crates, "ocx_setup": ["tests/test_self_*.py", "tests/test_gone.py"]}, cov_rows.verbs, cov_rows.security)
        dangling = {**cov_markers, "tests/test_push.py": (["package_psh"], [])}
        missing = Rows(dict(cov_escalates), cov_rows.verbs, cov_rows.security)
        stale_verb = Rows(cov_rows.crates, {**cov_rows.verbs, "gone.rs": "escalate"}, cov_rows.security)
        cases = [
            (coverage(rows=dead), "tests/test_gone.py"),
            (coverage(markers=dangling), "package_psh"),
            (coverage(rows=missing, markers={**cov_markers, "tests/test_self_setup.py": (["self_group/*"], [])}), "ocx_setup"),
            (coverage(members=cov_members | {"ocx_new"}), "ocx_new"),
            (coverage(rows=stale_verb), "gone.rs"),
        ]
        for found, needle in cases:
            if not any(needle in f for f in found):
                return f"expected a finding naming {needle!r}, got {found}"
        return None

    def marker_carried_forward_is_read() -> str | None:
        # The bare name carries the previous binding's command key forward,
        # and a list may hold two command marks.
        carried, carried_problems = read_markers(
            'import pytest\n\npytestmark = pytest.mark.command("a")\npytestmark = [pytestmark, pytest.mark.smoke]\n'
        )
        two, two_problems = read_markers(
            'import pytest\n\npytestmark = [pytest.mark.command("a"), pytest.mark.command("b", "c")]\n'
        )
        return expect(
            carried == ["a"] and carried_problems == [] and two == ["a", "b", "c"] and two_problems == [],
            f"a carried-forward key and a two-mark list must read: {carried} {carried_problems} {two} {two_problems}",
        )

    def security_manifest_escalates() -> str | None:
        # [security] runs before the manifest route: a feature toggle in a
        # security crate's Cargo.toml never touches Cargo.lock.
        live = load_rows(ROWS_FILE)
        for path, glob in (("crates/ocx_sign/Cargo.toml", "crates/ocx_sign/**"), ("crates/ocx_oci/README.md", "crates/ocx_oci/**")):
            plan = _plan_for([path], ws, root, rows=live)
            if plan.decision != "escalate" or plan.routes["manifests"] or f"[security] {glob}" not in plan.reason:
                return f"{path} must escalate by [security] {glob}, not route: {plan.as_json()}"
        return None

    def rowless_crate_escalates() -> str | None:
        rows = load_rows(root / "test" / "scoped_rows.toml")
        rowless = Rows({k: v for k, v in rows.crates.items() if k != "ocx_setup"}, rows.verbs, rows.security)
        plan = _plan_for(["crates/ocx_setup/src/lib.rs"], ws, root, rows=rowless)
        return expect(
            plan.decision == "escalate" and "ocx_setup: no [crates] row" in plan.reason,
            f"a changed crate with no row must escalate naming it: {plan.as_json()}",
        )

    def the_table_escalates_its_own_edit() -> str | None:
        # The table decides what every scoped run selects, so no route may
        # certify an edit to it: only the full verify (and its rows check) does.
        plan = _plan_for(["test/scoped_rows.toml"], ws, root)
        return expect(
            plan.decision == "escalate" and plan.reason.startswith("test/scoped_rows.toml:"),
            f"an edit to the table must escalate: {plan.as_json()}",
        )

    def a_rename_lists_both_paths() -> str | None:
        repo = tmp / "renames"
        (repo / ".github" / "workflows").mkdir(parents=True)
        git(repo, "init", "-q", "-b", "work")
        (repo / ".github" / "workflows" / "scan.yml").write_text("on: push\njobs: {}\n", encoding="utf-8")
        git(repo, "add", "-A")
        git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "A")
        base = git(repo, "rev-parse", "HEAD").strip()
        (repo / ".github" / "ISSUE_TEMPLATE").mkdir()
        git(repo, "mv", ".github/workflows/scan.yml", ".github/ISSUE_TEMPLATE/scan.yml")
        git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "B")
        changed = changed_paths(repo, base)
        return expect(
            changed == [".github/ISSUE_TEMPLATE/scan.yml", ".github/workflows/scan.yml"],
            f"a rename out of a [security] glob must list the removed path too: {changed}",
        )

    def required_security_is_a_floor() -> str | None:
        # One red per required glob: dropping any one of them from the table
        # is a coverage finding, so the per-glob routing reds cannot be
        # deleted along with their row.
        for glob in REQUIRED_SECURITY:
            rows = Rows(cov_rows.crates, cov_rows.verbs, [g for g in cov_rows.security if g != glob])
            if len(rows.security) != len(cov_rows.security) - 1:
                return f"the removal of {glob!r} did not land"
            found = coverage(rows=rows)
            if not any(glob in f and "requires" in f for f in found):
                return f"dropping {glob!r} from [security] must red, got {found}"
        return None

    def wide_wildcard_key_reds() -> str | None:
        # Only `<command key>*` or `<segment>_*` / `<segment>/*` may widen.
        for key in ("*", "p*", "*_*", "package_pus?", "[p]ackage_push"):
            found = coverage(markers={**cov_markers, "tests/test_other.py": ([key], [])})
            if len(found) != 1 or "single trailing" not in found[0]:
                return f"marker key {key!r} must red once as too wide, got {found}"
        return None

    def dead_security_glob_reds() -> str | None:
        # A rename that leaves a [security] glob matching nothing (login.rs →
        # auth.rs) reds, even though every string is still in the table.
        tracked = [path for path in cov_tracked if not path.endswith("login.rs")]
        return one_red(coverage(tracked=tracked), "command/login.rs matches no tracked file")

    def custom_build_script_escalates() -> str | None:
        # Built, not spelled: the dead-path sweep reads `crates/...` literals.
        script = (Path("crates") / "ocx_setup" / "gen" / "main.rs").as_posix()
        meta = _fixture_metadata(root)
        for pkg in meta["packages"]:
            if pkg["name"] == "ocx_setup":
                pkg["targets"] = [{"kind": ["custom-build"], "src_path": str(root / script)}]
        custom = parse_workspace(meta)
        plan = _plan_for([script], custom, root)
        control = _plan_for([script], ws, root)
        return expect(
            custom.build_scripts == frozenset({script})
            and plan.decision == "escalate"
            and "build script" in plan.reason
            and control.decision == "scoped",
            f"a manifest-named build script must escalate: {plan.as_json()} (control {control.decision})",
        )

    def escalate_rows_match_the_constant() -> str | None:
        rows = Rows({**cov_rows.crates, "ocx_setup": "escalate"}, cov_rows.verbs, cov_rows.security)
        markers = {**cov_markers, "tests/test_self_setup.py": (["self_group/*"], [])}
        found = coverage(rows=rows, markers=markers)
        return expect(
            any("TABLE_ESCALATES" in f for f in found),
            f"an escalate row TABLE_ESCALATES does not name must red: {found}",
        )

    def a_single_when_glob_is_read() -> str | None:
        found = coverage(hex_text='- role: reviewer:security\n  when: ".github/workflows/**"\n')
        return expect(found == [], f"a brace-less single when: glob is a glob, not a parse failure: {found}")

    def coverage_live_green() -> str | None:
        found = live_coverage(REPO_ROOT)
        return expect(found == [], f"the live tree must pass --check-coverage: {found}")

    def crates_and_routes_mix() -> str | None:
        plan = _plan_for(["crates/ocx_setup/src/lib.rs", ".claude/rules/x.md"], ws, root)
        return expect(
            plan.decision == "scoped" and plan.routes["claude"] == [".claude/rules/x.md"],
            f"a crate plus a routed path is scoped with the route kept: {plan.as_json()}",
        )

    def nothing_changed_is_routed_with_nothing() -> str | None:
        plan = _plan_for([], ws, root)
        return expect(
            plan.decision == "routed" and not any(plan.routes.values()) and plan.crates == [],
            f"an empty change set is routed with no routes: {plan.as_json()}",
        )

    def escalation_keeps_crates_informational() -> str | None:
        plan = _plan_for(["taskfile.yml", "crates/ocx_setup/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate" and plan.crates == ["ocx_setup"],
            f"escalation still lists the changed crates: {plan.as_json()}",
        )

    def mark_round_trip() -> str | None:
        repo = tmp / "marks"
        repo.mkdir()
        git(repo, "init", "-q", "-b", "work")
        (repo / "a").write_text("a\n", encoding="utf-8")
        git(repo, "add", "a")
        git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "A")
        sha_a = git(repo, "rev-parse", "HEAD").strip()
        mark = repo / "mark.json"
        if read_mark(mark) != {}:
            return "a missing mark must read as {}"
        mark.write_text("1758000000\n", encoding="utf-8")
        if read_mark(mark) != {}:
            return "a bare-integer stamp must read as {}"
        mark.write_text("not json", encoding="utf-8")
        if read_mark(mark) != {}:
            return "garbage must read as {}"
        top = worktree_id(repo)
        if top != str(repo.resolve()):
            return f"the writer identity must be the realpath'd working tree: {top}"
        full = write_mark(mark, "full", [], sha_a, top)
        if full.get("scope") != "full" or full.get("head") != sha_a or "full_head" in full:
            return f"full mark wrong: {full}"
        if not isinstance(full.get("timestamp"), int) or full.get("crates") != []:
            return f"full mark fields wrong: {full}"
        # R17: without this the mark says nothing about WHICH tree earned it,
        # and a sibling worktree at the same HEAD reads it as its own.
        if full.get("toplevel") != top:
            return f"the mark must record the tree that wrote it: {full}"
        if full_head_of(read_mark(mark)) != sha_a:
            return "full_head_of(full mark) must be its head"
        (repo / "b").write_text("b\n", encoding="utf-8")
        git(repo, "add", "b")
        git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "B")
        sha_b = git(repo, "rev-parse", "HEAD").strip()
        scoped = write_mark(mark, "scoped", ["ocx_setup"], sha_b, top)
        if scoped.get("full_head") != sha_a or scoped.get("crates") != ["ocx_setup"]:
            return f"scoped mark must carry the full head forward: {scoped}"
        if scoped.get("toplevel") != top:
            return f"a scoped mark must record its tree too: {scoped}"
        scoped = write_mark(mark, "scoped", ["ocx_shell"], sha_b, top)
        if scoped.get("full_head") != sha_a:
            return f"a second scoped mark must keep carrying the full head: {scoped}"
        if resolve_base(repo, mark) != (sha_a, "full-mark"):
            return f"base must be the full mark's head: {resolve_base(repo, mark)}"
        git(repo, "update-ref", "refs/remotes/origin/main", sha_a)
        mark.write_text(json.dumps({**scoped, "full_head": "0" * 40}), encoding="utf-8")
        if resolve_base(repo, mark) != (sha_a, "merge-base(origin/main)"):
            return f"an unreachable full head must fall back to the merge base: {resolve_base(repo, mark)}"
        mark.unlink()
        if resolve_base(repo, mark) != (sha_a, "merge-base(origin/main)"):
            return "no mark must fall back to the merge base"
        (repo / "a").write_text("a2\n", encoding="utf-8")
        (repo / "c").write_text("c\n", encoding="utf-8")
        changed = changed_paths(repo, sha_a)
        return expect(
            changed == ["a", "b", "c"], f"changed paths must span commits and the tree: {changed}"
        )

    def mark_home_follows_the_project_dir() -> str | None:
        """The mark lands where the hook reads it, not where the script sits.

        Driven end to end, because the defect is invisible in process: a
        stamp from a worktree prints the JSON it wrote and looks like it
        worked while the gate reads a different file. So this builds a
        throwaway project, adds a real linked worktree of it, runs the
        script *from the worktree* and asserts on the two paths.
        """
        proj = tmp / "markhome"
        (proj / "scripts").mkdir(parents=True)
        (proj / ".claude").mkdir()
        (proj / ".claude" / ".keep").write_text("", encoding="utf-8")
        for name in ("scoped_gate.py", "_git.py"):
            (proj / "scripts" / name).write_bytes((REPO_ROOT / "scripts" / name).read_bytes())
        git(proj, "init", "-q", "-b", "work")
        git(proj, "add", "-A")
        git(proj, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "A")
        wt = tmp / "markhome-wt"
        git(proj, "worktree", "add", "-q", str(wt), "-b", "side")
        script = wt / "scripts" / "scoped_gate.py"
        at_project = proj / ".claude" / "hooks" / ".state" / "commit-verified"
        at_worktree = wt / ".claude" / "hooks" / ".state" / "commit-verified"

        def stamp(project_dir: str | None, *extra: str) -> subprocess.CompletedProcess[str]:
            env = {k: v for k, v in os.environ.items() if k != "CLAUDE_PROJECT_DIR"}
            if project_dir is not None:
                env["CLAUDE_PROJECT_DIR"] = project_dir
            return subprocess.run(
                [sys.executable, str(script), "--mark", "scoped", *extra],
                capture_output=True, text=True, env=env, cwd=str(wt), check=False,
            )

        # (a) CLAUDE_PROJECT_DIR set, cwd inside a linked worktree of it.
        run = stamp(str(proj))
        if run.returncode != 0:
            return f"--mark failed under a project dir: {run.stderr.strip()}"
        if not at_project.is_file():
            return f"the mark must land at the project dir, not at {list(wt.rglob('commit-verified'))}"
        if at_worktree.exists():
            return "the mark must NOT land in the worktree when a project dir is set"
        # (b) the reader's contract: the same shape, this worktree's HEAD.
        mark = json.loads(at_project.read_text(encoding="utf-8"))
        head = git(wt, "rev-parse", "HEAD").strip()
        if set(mark) != {"timestamp", "head", "scope", "crates", "toplevel", "tree", "nocache"}:
            return f"mark key set drifted from the reader's contract: {sorted(mark)}"
        # A `release:` commit reads `nocache`: false unless the run said so.
        if mark["nocache"] is not False:
            return f"a mark written without --nocache must record nocache false: {mark}"
        if stamp(str(proj), "--nocache").returncode != 0 or json.loads(
            at_project.read_text(encoding="utf-8")
        )["nocache"] is not True:
            return f"--nocache must record nocache true: {at_project.read_text(encoding='utf-8')}"
        # C-017: no `--mark-precheck` ran, so no run proved the index is the
        # tree it built — the mark carries no merge-eligible tree.
        if mark["tree"] is not None:
            return f"a mark with no precheck behind it must carry no tree: {mark}"
        if mark["head"] != head or mark["scope"] != "scoped" or not isinstance(mark["timestamp"], int):
            return f"mark must certify the worktree's HEAD {head}: {mark}"
        # The mark HOME is the project dir (DX-52) but the writer identity is
        # the tree that ran: shared file, one tree named in it.
        if mark["toplevel"] != str(wt.resolve()):
            return f"mark must name the worktree that wrote it, not the project dir: {mark}"
        # (c) unset, the CLI and CI shape: the fallback is still REPO_ROOT.
        before = at_project.read_text(encoding="utf-8")
        run = stamp(None)
        if run.returncode != 0:
            return f"--mark failed with no project dir: {run.stderr.strip()}"
        if not at_worktree.is_file():
            return "with CLAUDE_PROJECT_DIR unset the mark must fall back to REPO_ROOT"
        if at_project.read_text(encoding="utf-8") != before:
            return "the fallback must not touch the project dir's mark"
        return None

    def metadata_failure_is_loud() -> str | None:
        bare = tmp / "bare"
        bare.mkdir()
        try:
            cargo_metadata(bare)
        except SystemExit as stop:
            return expect("cargo metadata failed" in str(stop), f"wrong failure text: {stop}")
        return "cargo metadata on a directory without a manifest must raise SystemExit"

    def the_manifest_route_reaches_a_workspace_compile() -> str | None:
        """D40: the route's steps must compile what a manifest decides.

        `classify` sends a `crates/<c>/Cargo.toml` edit to `routed`, and the
        guards target that route runs compiles `ocx_test_support` alone. Since
        `Cargo.lock` carries `name`/`version`/`source`/`checksum`/`dependencies`
        and no feature data, a `[features]` edit never drags the lock into the
        change set and never escalates — so the workspace compile is the only
        step that can go red on it, and what this pins is the condition that
        reaches it. The red/green of the step itself is the gate run.
        """
        text = ROOT_TASKFILE.read_text(encoding="utf-8")
        steps = re.findall(r"- if: '([^']*)'\n\s+cmd: (cargo [^\n]+)", text)
        compile_conditions = [
            condition for condition, cmd in steps if cmd.startswith("cargo check --workspace")
        ]
        guard_conditions = [
            condition for condition, cmd in steps if "--test workspace_structure" in cmd
        ]
        if len(compile_conditions) != 1 or len(guard_conditions) != 1:
            return (
                "verify:scoped must run exactly one `cargo check --workspace` and one guards"
                f" step: {compile_conditions} {guard_conditions}"
            )
        if "ROUTE_MANIFESTS" not in guard_conditions[0]:
            return f"the guards step must be the manifest route's: {guard_conditions[0]}"
        return expect(
            "ROUTE_MANIFESTS" in compile_conditions[0],
            f"a manifest route that compiles nothing certifies nothing (D40): {compile_conditions[0]}",
        )

    def table_rows_agree() -> str | None:
        rows = load_rows(ROWS_FILE)
        escalates = {crate for crate, row in rows.crates.items() if row == "escalate"}
        return expect(
            escalates == set(TABLE_ESCALATES),
            f"test/scoped_rows.toml escalate rows {sorted(escalates)} != TABLE_ESCALATES {sorted(TABLE_ESCALATES)}",
        )

    # -- C-017 AM-8: `--mark` under a merge, end to end --------------------

    def _commit(repo: Path, message: str) -> None:
        git(repo, "add", "-A")
        git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", message)

    def _am8_project(name: str, *, merging: bool) -> Path:
        """A scratch project whose `work` has `side` merged in, paused, or not.

        The mark home and `__pycache__` are ignored exactly as the real
        repository ignores them, so the only dirt is what a case plants.
        """
        proj = tmp / name
        (proj / "scripts").mkdir(parents=True)
        (proj / ".claude").mkdir()
        (proj / ".gitignore").write_text(".claude/hooks/.state/\n__pycache__/\n", encoding="utf-8")
        for script in ("scoped_gate.py", "_git.py"):
            (proj / "scripts" / script).write_bytes((REPO_ROOT / "scripts" / script).read_bytes())
        git(proj, "init", "-q", "-b", "work")
        _commit(proj, "A")
        git(proj, "switch", "-q", "-c", "side")
        (proj / "side_file.txt").write_text("side\n", encoding="utf-8")
        _commit(proj, "side")
        git(proj, "switch", "-q", "work")
        (proj / "trunk_file.txt").write_text("trunk\n", encoding="utf-8")
        _commit(proj, "trunk")
        if merging:
            git(proj, "merge", "--no-ff", "--no-commit", "side")
            if not (Path(git(proj, "rev-parse", "--absolute-git-dir").strip()) / "MERGE_HEAD").exists():
                raise SystemExit("fixture: the paused merge left no MERGE_HEAD")
        return proj

    def _project_mark(proj: Path) -> Path:
        return proj / ".claude" / "hooks" / ".state" / "commit-verified"

    def _mark_cli(proj: Path, *args: str) -> subprocess.CompletedProcess[str]:
        env = {k: v for k, v in os.environ.items() if k != "GIT_INDEX_FILE"}
        env["CLAUDE_PROJECT_DIR"] = str(proj)
        return subprocess.run(
            [sys.executable, str(proj / "scripts" / "scoped_gate.py"), "--mark", *args],
            capture_output=True, encoding="utf-8", env=env, cwd=str(proj), check=False,
        )

    remedy = "stage or remove them, then re-run `task verify`"

    def _gate_cli(proj: Path, *args: str) -> subprocess.CompletedProcess[str]:
        env = {k: v for k, v in os.environ.items() if k != "GIT_INDEX_FILE"}
        env["CLAUDE_PROJECT_DIR"] = str(proj)
        return subprocess.run(
            [sys.executable, str(proj / "scripts" / "scoped_gate.py"), *args],
            capture_output=True, encoding="utf-8", env=env, cwd=str(proj), check=False,
        )

    def _start_snapshot(proj: Path) -> Path:
        return Path(git(proj, "rev-parse", "--absolute-git-dir").strip()) / VERIFY_START

    def fix1_precheck_refuses_a_dirty_merge_up_front() -> str | None:
        """The AM-8 refusal at the START of `task verify`, not after the suite."""
        proj = _am8_project("pre-dirty", merging=True)
        (proj / "trunk_file.txt").write_text("an unstaged repair\n", encoding="utf-8")
        run = _gate_cli(proj, "--mark-precheck")
        if run.returncode != 1 or "trunk_file.txt" not in run.stderr or remedy not in run.stderr:
            return f"--mark-precheck on a dirty merge must exit 1 naming the path: {run.returncode} {run.stderr!r}"
        if _start_snapshot(proj).exists():
            return "a refused precheck must leave no start snapshot"
        git(proj, "checkout", "--", "trunk_file.txt")
        run = _gate_cli(proj, "--mark-precheck")
        if run.returncode != 0:
            return f"--mark-precheck on a clean merge must exit 0: {run.returncode} {run.stderr!r}"
        snap = _start_snapshot(proj)
        return expect(
            snap.is_file() and snap.read_text(encoding="utf-8").strip() == git(proj, "write-tree").strip(),
            "the precheck must snapshot the index tree it saw",
        )

    def fix1_a_tree_staged_during_the_run_refuses_the_mark() -> str | None:
        """TOCTOU: what verify built is the tree at its start; a later `git add` is not it."""
        proj = _am8_project("pre-toctou", merging=True)
        if _gate_cli(proj, "--mark-precheck").returncode != 0:
            return "fixture: the precheck must pass on a clean merge"
        (proj / "trunk_file.txt").write_text("staged mid-run\n", encoding="utf-8")
        git(proj, "add", "trunk_file.txt")
        run = _mark_cli(proj, "full")
        if run.returncode != 1 or "since `task verify` started" not in run.stderr:
            return f"--mark after a mid-run stage must exit 1 and say why: {run.returncode} {run.stderr!r}"
        return expect(not _project_mark(proj).exists(), "a refused --mark must write no mark")

    def fix1_a_merge_mark_needs_the_precheck() -> str | None:
        """No snapshot under a merge = no evidence of what was built: refuse."""
        proj = _am8_project("pre-missing", merging=True)
        run = _mark_cli(proj, "full")
        if run.returncode != 1 or "--mark-precheck" not in run.stderr:
            return f"--mark under a merge with no start snapshot must exit 1: {run.returncode} {run.stderr!r}"
        return expect(not _project_mark(proj).exists(), "a refused --mark must write no mark")

    def fix1_precheck_outside_a_merge_is_a_no_op() -> str | None:
        proj = _am8_project("pre-no-merge", merging=False)
        (proj / "trunk_file.txt").write_text("an unstaged edit\n", encoding="utf-8")
        run = _gate_cli(proj, "--mark-precheck")
        if run.returncode != 0:
            return f"--mark-precheck outside a merge must exit 0: {run.returncode} {run.stderr!r}"
        return expect(not _start_snapshot(proj).exists(), "outside a merge no snapshot is written")

    def am8_unstaged_edit_under_a_merge_refuses() -> str | None:
        proj = _am8_project("am8-unstaged", merging=True)
        mark = _project_mark(proj)
        write_mark(mark, "scoped", [], git(proj, "rev-parse", "HEAD").strip(), str(proj.resolve()))
        before = mark.read_bytes()
        (proj / "trunk_file.txt").write_text("an unstaged repair\n", encoding="utf-8")
        run = _mark_cli(proj, "full")
        if run.returncode != 1:
            return f"--mark full under MERGE_HEAD with an unstaged edit must exit 1: {run.returncode} {run.stderr!r}"
        if "trunk_file.txt" not in run.stderr or remedy not in run.stderr:
            return f"the refusal must name the path and the remedy: {run.stderr!r}"
        return expect(mark.read_bytes() == before, "a refused --mark must leave the existing mark unchanged")

    def am8_untracked_file_under_a_merge_refuses() -> str | None:
        proj = _am8_project("am8-untracked", merging=True)
        (proj / "planted_untracked.txt").write_text("untracked\n", encoding="utf-8")
        run = _mark_cli(proj, "full")
        if run.returncode != 1:
            return f"--mark full under MERGE_HEAD with an untracked file must exit 1: {run.returncode} {run.stderr!r}"
        if "planted_untracked.txt" not in run.stderr or remedy not in run.stderr:
            return f"the refusal must name the path and the remedy: {run.stderr!r}"
        return expect(not _project_mark(proj).exists(), "a refused --mark must write no mark")

    def am8_unstaged_edit_outside_a_merge_marks() -> str | None:
        """Control: outside a merge a dirty tree still marks — but earns no merge-eligible tree."""
        proj = _am8_project("am8-control", merging=False)
        (proj / "trunk_file.txt").write_text("an unstaged edit\n", encoding="utf-8")
        if _gate_cli(proj, "--mark-precheck").returncode != 0:
            return "--mark-precheck outside a merge must exit 0"
        run = _mark_cli(proj, "full")
        if run.returncode != 0:
            return f"--mark full outside a merge must exit 0: {run.returncode} {run.stderr!r}"
        mark = json.loads(_project_mark(proj).read_text(encoding="utf-8"))
        return expect(
            mark.get("scope") == "full" and mark.get("tree") is None,
            f"a run over a working tree that differs from the index proves no tree: {mark}",
        )

    def a_clean_verify_outside_a_merge_records_its_tree() -> str | None:
        """Green: precheck and mark both on a tree equal to its index → `tree` is that tree."""
        proj = _am8_project("clean-outside", merging=False)
        if _gate_cli(proj, "--mark-precheck").returncode != 0:
            return "--mark-precheck on a clean tree must exit 0"
        if _mark_cli(proj, "full").returncode != 0:
            return "--mark full on a clean tree must exit 0"
        mark = json.loads(_project_mark(proj).read_text(encoding="utf-8"))
        if mark.get("tree") != git(proj, "write-tree").strip():
            return f"a clean verify must record the index tree: {mark}"
        # One snapshot, one mark: the second mark has no run behind it.
        if _mark_cli(proj, "full").returncode != 0:
            return "a second --mark full outside a merge must still exit 0"
        mark = json.loads(_project_mark(proj).read_text(encoding="utf-8"))
        return expect(mark.get("tree") is None, f"a mark with no precheck behind it proves no tree: {mark}")

    def a_tree_dirtied_during_the_run_records_no_tree() -> str | None:
        """Clean at the start is not enough: the tree at mark time must still be the index."""
        proj = _am8_project("dirty-midrun", merging=False)
        if _gate_cli(proj, "--mark-precheck").returncode != 0:
            return "--mark-precheck on a clean tree must exit 0"
        (proj / "trunk_file.txt").write_text("edited mid-run\n", encoding="utf-8")
        if _mark_cli(proj, "full").returncode != 0:
            return "--mark full outside a merge must exit 0"
        mark = json.loads(_project_mark(proj).read_text(encoding="utf-8"))
        return expect(mark.get("tree") is None, f"a tree dirtied mid-run proves no tree: {mark}")

    def a_premerge_mark_of_a_staged_foreign_tree_admits_no_merge() -> str | None:
        """The adversary's repro: stage `wp`'s tree over an untouched working tree, verify, merge.

        The run built HEAD's working tree while the index held `wp`'s tree;
        the merge of `wp` then has exactly that index tree. The mark must not
        carry it, so the merge-commit clause refuses — while the same merge,
        verified during the merge, is admitted.
        """
        # The reader, in-process; imported here because it imports this module.
        from commit_gate import merge_commit_refusal

        proj = _am8_project("adv-staged", merging=False)
        git(proj, "switch", "-q", "-c", "wp")
        (proj / "regression.txt").write_text("a regression\n", encoding="utf-8")
        _commit(proj, "wp")
        git(proj, "switch", "-q", "work")
        git(proj, "restore", "--source=wp", "--staged", ".")
        staged = git(proj, "write-tree").strip()
        if staged != git(proj, "rev-parse", "wp^{tree}").strip():
            return "fixture: the index must hold wp's tree"
        if _gate_cli(proj, "--mark-precheck").returncode != 0:
            return "--mark-precheck outside a merge must exit 0"
        run = _mark_cli(proj, "full")
        if run.returncode != 0:
            return f"--mark full outside a merge must exit 0: {run.returncode} {run.stderr!r}"
        git(proj, "restore", "--source=HEAD", "--staged", "--worktree", ".")
        git(proj, "merge", "--no-ff", "--no-commit", "wp")
        if git(proj, "write-tree").strip() != staged:
            return "fixture: the merge's index tree must equal the tree staged before the run"
        mark = json.loads(_project_mark(proj).read_text(encoding="utf-8"))
        if merge_commit_refusal(mark, str(proj)) is None:
            return f"a mark whose run built another working tree must not admit the merge: {mark}"
        # Green: the same merge, verified now, is admitted.
        if _gate_cli(proj, "--mark-precheck").returncode != 0:
            return "--mark-precheck on the clean paused merge must exit 0"
        if _mark_cli(proj, "full").returncode != 0:
            return "--mark full on the clean paused merge must exit 0"
        mark = json.loads(_project_mark(proj).read_text(encoding="utf-8"))
        refusal = merge_commit_refusal(mark, str(proj))
        return expect(refusal is None, f"a merge-time verify must admit the merge: {refusal}")

    def am8_a_clean_merge_marks_the_merged_tree() -> str | None:
        proj = _am8_project("am8-clean-merge", merging=True)
        pre = _gate_cli(proj, "--mark-precheck")
        if pre.returncode != 0:
            return f"--mark-precheck on a clean paused merge must exit 0: {pre.returncode} {pre.stderr!r}"
        run = _mark_cli(proj, "full")
        if run.returncode != 0:
            return f"--mark full on a clean paused merge must exit 0: {run.returncode} {run.stderr!r}"
        mark = json.loads(_project_mark(proj).read_text(encoding="utf-8"))
        merged = git(proj, "write-tree").strip()
        if merged == git(proj, "rev-parse", "HEAD^{tree}").strip():
            return "fixture: the merged index must differ from HEAD's tree"
        return expect(mark.get("tree") == merged, f"the mark must carry the merged index tree {merged}: {mark}")

    def c018_log_run_survives_a_plan_failure() -> str | None:
        """`--mark scoped --log-run` where `make_plan` cannot run: warn, no record, still mark."""
        proj = _am8_project("log-run-no-plan", merging=False)
        run = _mark_cli(proj, "scoped", "--log-run")
        if run.returncode != 0:
            return f"a failed plan must not fail the mark: {run.returncode} {run.stderr!r}"
        if not run.stderr.strip():
            return "a failed plan must warn on stderr"
        if not _project_mark(proj).is_file():
            return "the mark must still be written"
        common = Path(git(proj, "rev-parse", "--path-format=absolute", "--git-common-dir").strip())
        return expect(
            not (common / "ocx-gate" / RUN_LOG).exists(),
            "a failed plan must append no run record",
        )

    # -- C-018: the scoped-run log --------------------------------------------

    def _scratch_repo(name: str) -> Path:
        repo = tmp / name
        repo.mkdir()
        git(repo, "init", "-q", "-b", "work")
        (repo / "seed").write_text("seed\n", encoding="utf-8")
        _commit(repo, "seed")
        return repo

    def _tree_of_add_all(repo: Path) -> str:
        """What `git add -A` would stage, computed here independently of the stub."""
        index = Path(git(repo, "rev-parse", "--git-path", "index").strip())
        index = index if index.is_absolute() else repo / index
        scratch = tmp / f"{repo.name}-scratch.index"
        scratch.write_bytes(index.read_bytes())
        env = {**os.environ, "GIT_INDEX_FILE": str(scratch)}
        for args in (("add", "-A"), ("write-tree",)):
            done = subprocess.run(
                ["git", "-C", str(repo), *args], capture_output=True, encoding="utf-8", env=env, check=True
            )
        return done.stdout.strip()

    def c018_run_log_lands_in_the_common_dir() -> str | None:
        repo = _scratch_repo("runlog")
        wt = tmp / "runlog-wt"
        git(repo, "worktree", "add", "-q", str(wt), "-b", "wp")
        (wt / "seed").write_text("unstaged\n", encoding="utf-8")
        (wt / "fresh").write_text("untracked\n", encoding="utf-8")
        index_before = git(wt, "write-tree").strip()
        status_before = git(wt, "status", "--porcelain")
        expected_wt_tree = _tree_of_add_all(wt)
        common = Path(git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir").strip())
        if gate_log_dir(wt) != common / "ocx-gate" or gate_log_dir(repo) != common / "ocx-gate":
            return f"gate_log_dir must be <common dir>/ocx-gate from every worktree: {gate_log_dir(wt)}"
        record = log_scoped_run(wt, ["tests/test_b*.py", "tests/test_a*.py"])
        log = common / "ocx-gate" / RUN_LOG
        if not log.is_file():
            return f"the run log must land at {log}"
        lines = log.read_text(encoding="utf-8").splitlines()
        if len(lines) != 1:
            return f"one run must append exactly one line: {lines}"
        line = json.loads(lines[0])
        if set(line) != {"ts", "worktree", "tree", "worktree_tree", "head", "acceptance_globs"}:
            return f"run record key set drifted: {sorted(line)}"
        if line != record:
            return f"the returned record must be the line written: {record} != {line}"
        want = {
            "worktree": str(wt.resolve()),
            "tree": index_before,
            "worktree_tree": expected_wt_tree,
            "head": git(wt, "rev-parse", "HEAD").strip(),
            "acceptance_globs": ["tests/test_a*.py", "tests/test_b*.py"],
        }
        wrong = {k: (line[k], v) for k, v in want.items() if line[k] != v}
        if wrong:
            return f"run record fields wrong (got, want): {wrong}"
        if not isinstance(line["ts"], (int, float)):
            return f"`ts` must be a number: {line['ts']!r}"
        if expected_wt_tree == index_before:
            return "fixture: the working tree must differ from the index"
        if git(wt, "write-tree").strip() != index_before or git(wt, "status", "--porcelain") != status_before:
            return "computing worktree_tree must leave the real index untouched"
        log_scoped_run(wt, ["tests/test_c*.py"])
        again = log.read_text(encoding="utf-8").splitlines()
        return expect(
            len(again) == 2 and again[0] == lines[0], f"a second run must append, never truncate: {again}"
        )

    def c018_append_jsonl_unique_on_skips() -> str | None:
        path = tmp / "unique" / "log.jsonl"
        path.parent.mkdir()
        path.write_text('{"a": 0}\n', encoding="utf-8")
        if append_jsonl(path, {"a": 1, "b": 2, "c": 3}) is not True:
            return "a plain append must return True"
        before = path.read_bytes()
        if append_jsonl(path, {"a": 1, "b": 2, "c": 99}, unique_on=("a", "b")) is not False:
            return "a duplicate on the unique keys must return False"
        if path.read_bytes() != before:
            return "a skipped append must write nothing"
        if append_jsonl(path, {"a": 1, "b": 3, "c": 3}, unique_on=("a", "b")) is not True:
            return "a line differing on a unique key must append"
        lines = path.read_text(encoding="utf-8").splitlines()
        if lines[0] != '{"a": 0}' or len(lines) != 3:
            return f"appends must never truncate: {lines}"
        return expect(
            list(json.loads(lines[1])) == sorted(json.loads(lines[1])), f"lines are sorted-keys JSON: {lines[1]}"
        )

    # -- C-018: the escape recorder --------------------------------------------

    failing_xml = (
        '<testsuites><testsuite name="s" tests="1"><testcase classname="c" name="n">'
        '<failure message="boom">boom</failure></testcase></testsuite></testsuites>\n'
    )
    erroring_xml = (
        '<testsuites><testsuite name="s" tests="1"><testcase classname="c" name="n">'
        '<error message="boom">boom</error></testcase></testsuite></testsuites>\n'
    )
    passing_xml = (
        '<testsuites><testsuite name="s" tests="1"><testcase classname="c" name="n"/>'
        "</testsuite></testsuites>\n"
    )

    def _escape_repo(name: str) -> tuple[Path, Path, Path]:
        """(main worktree, linked worktree `wp`, junit dir) — fresh per case."""
        repo = _scratch_repo(name)
        wp = tmp / f"{name}-wp"
        git(repo, "worktree", "add", "-q", str(wp), "-b", "wp")
        junit = tmp / f"{name}-junit"
        for module, body in (("test_planted", failing_xml), ("test_passing", passing_xml)):
            (junit / module).mkdir(parents=True)
            (junit / module / "junit.xml").write_text(body, encoding="utf-8")
        return repo, wp, junit

    def _wp_commit(wp: Path, name: str = "wp_change") -> None:
        (wp / name).write_text(f"{name}\n", encoding="utf-8")
        _commit(wp, name)

    def _merge_wp(repo: Path) -> str:
        git(repo, "merge", "--no-ff", "--no-commit", "wp")
        return git(repo, "rev-parse", "MERGE_HEAD^{tree}").strip()

    def _record(repo: Path, junit: Path, since: float | None = None) -> tuple[int, list[str]]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = record_escapes(repo, junit, since)
        return rc, [line for line in out.getvalue().splitlines() if line.strip()]

    def _escapes(repo: Path) -> list[dict]:
        path = gate_log_dir(repo) / ESCAPES_LOG
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    hex40 = re.compile(r"[0-9a-f]{40}")

    def c018_a_missed_module_is_one_escape() -> str | None:
        repo, wp, junit = _escape_repo("esc-missed")
        _wp_commit(wp)
        log_scoped_run(wp, ["tests/test_other*.py"])
        merge_tree = _merge_wp(repo)
        rc, lines = _record(repo, junit)
        if rc != 0:
            return f"the recorder must return 0: {rc}"
        if lines != ["escape: test_planted"]:
            return f"stdout must be exactly one escape line: {lines}"
        escapes = _escapes(repo)
        if len(escapes) != 1:
            return f"exactly one escapes.jsonl line: {escapes}"
        esc = escapes[0]
        if set(esc) != {"ts", "tree", "head", "merge_head_tree", "module", "scoped_globs"}:
            return f"escape record key set drifted: {sorted(esc)}"
        if esc["module"] != "test_planted" or esc["merge_head_tree"] != merge_tree:
            return f"escape record must name the module and MERGE_HEAD^{{tree}} {merge_tree}: {esc}"
        if esc["scoped_globs"] != ["tests/test_other*.py"]:
            return f"escape record must carry the record's globs: {esc}"
        if not all(isinstance(esc[k], str) and hex40.fullmatch(esc[k]) for k in ("tree", "head")):
            return f"`tree` and `head` must be full object ids: {esc}"
        return expect((junit / ESCAPE_MARKER).is_file(), "an escape must write the run's marker file")

    def c018_a_record_on_another_tree_is_unattributed() -> str | None:
        repo, wp, junit = _escape_repo("esc-other-tree")
        _wp_commit(wp, "first")
        log_scoped_run(wp, ["tests/test_other*.py"])
        _wp_commit(wp, "second")  # the tip moves past the logged tree
        _merge_wp(repo)
        rc, lines = _record(repo, junit)
        if rc != 0 or lines != ["unattributed: test_planted"]:
            return f"a record on another tree must be unattributed: rc={rc} {lines}"
        if _escapes(repo):
            return f"unattributed must append no escape: {_escapes(repo)}"
        return expect(not (junit / ESCAPE_MARKER).exists(), "unattributed must write no marker")

    def c018_a_record_that_selected_the_module_is_no_escape() -> str | None:
        repo, wp, junit = _escape_repo("esc-selected")
        _wp_commit(wp)
        log_scoped_run(wp, ["tests/test_plan*.py"])
        _merge_wp(repo)
        rc, lines = _record(repo, junit)
        if rc != 0 or any("test_planted" in line for line in lines):
            return f"a module the scoped run selected is neither escape nor unattributed: rc={rc} {lines}"
        if _escapes(repo):
            return f"no escape may be appended: {_escapes(repo)}"
        return expect(not (junit / ESCAPE_MARKER).exists(), "no marker for a selected module")

    def c018_no_merge_head_is_unattributed() -> str | None:
        repo, wp, junit = _escape_repo("esc-no-merge")
        _wp_commit(wp)
        log_scoped_run(wp, ["tests/test_other*.py"])
        rc, lines = _record(repo, junit)
        if rc != 0 or lines != ["unattributed: test_planted"]:
            return f"with no MERGE_HEAD every failure is unattributed: rc={rc} {lines}"
        if _escapes(repo):
            return f"no escape may be appended: {_escapes(repo)}"
        return expect(not (junit / ESCAPE_MARKER).exists(), "no marker with no MERGE_HEAD")

    def c018_a_run_logged_before_staging_matches_on_worktree_tree() -> str | None:
        """The realistic flow: verify:scoped, then stage, then commit."""
        repo, wp, junit = _escape_repo("esc-prestage")
        (wp / "wp_change").write_text("wp_change\n", encoding="utf-8")
        record = log_scoped_run(wp, ["tests/test_other*.py"])
        _commit(wp, "wp_change")
        merge_tree = _merge_wp(repo)
        if record["tree"] == merge_tree or record["worktree_tree"] != merge_tree:
            return f"fixture: only worktree_tree may equal the tip tree {merge_tree}: {record}"
        rc, lines = _record(repo, junit)
        if rc != 0 or lines != ["escape: test_planted"]:
            return f"a record matching on worktree_tree must attribute the escape: rc={rc} {lines}"
        return expect((junit / ESCAPE_MARKER).is_file(), "the escape must write the marker")

    def c018_recording_twice_keeps_one_escape() -> str | None:
        repo, wp, junit = _escape_repo("esc-twice")
        _wp_commit(wp)
        log_scoped_run(wp, ["tests/test_other*.py"])
        _merge_wp(repo)
        _record(repo, junit)
        _record(repo, junit)
        escapes = _escapes(repo)
        return expect(len(escapes) == 1, f"a re-run on the same merge must not duplicate: {escapes}")

    def c018_a_report_older_than_since_is_ignored() -> str | None:
        repo, wp, junit = _escape_repo("esc-since")
        (junit / "test_errored").mkdir()
        (junit / "test_errored" / "junit.xml").write_text(erroring_xml, encoding="utf-8")
        if failing_modules(junit) != ["test_errored", "test_planted"]:
            return f"a <failure> and an <error> both fail a module, passing does not: {failing_modules(junit)}"
        stale = time.time() - 3600
        for module in ("test_planted", "test_errored"):
            os.utime(junit / module / "junit.xml", (stale, stale))
        since = time.time() - 60
        if failing_modules(junit, since) != []:
            return f"reports older than `since` must be ignored: {failing_modules(junit, since)}"
        _wp_commit(wp)
        log_scoped_run(wp, ["tests/test_other*.py"])
        _merge_wp(repo)
        rc, lines = _record(repo, junit, since)
        if rc != 0 or lines:
            return f"a stale failing report is not this run's: rc={rc} {lines}"
        return expect(
            not _escapes(repo) and not (junit / ESCAPE_MARKER).exists(), "a stale report records nothing"
        )

    return [
        ("path→crate map", crate_map),
        ("reverse-dependent counts", rdeps),
        ("hub predicate", hubs),
        ("routing table", routes),
        ("nested taskfile escalates", nested_taskfile_escalates),
        ("unlisted .claude/ subtree escalates", unlisted_claude_subtree_escalates),
        ("scripts/** routes to the self-tests and escalates", scripts_route_and_escalate),
        ("crate manifest/README routes to the workspace guards", manifest_routes_without_escalating),
        ("crate source stays scoped beside a manifest", crate_source_is_still_scoped_beside_a_manifest),
        ("a manifest never causes the escalation", a_manifest_beside_an_escalating_path_does_not_run_the_guards_twice),
        ("unrouted paths escalate by name", unrouted_paths_escalate),
        ("non-hub crate is scoped", non_hub_crate_is_scoped),
        ("ecosystem crate escalates", ecosystem_crate_escalates),
        ("hub by count escalates", hub_by_count_escalates),
        ("hub at the boundary escalates", hub_at_the_boundary_escalates),
        ("test:scoped escalate row escalates", table_escalate_row_escalates),
        ("C-014 a marked verb is scoped to its modules", marked_verb_is_scoped),
        ("C-014 a *_common.rs file escalates ([verbs])", common_file_escalates),
        ("C-014 an unmarked command file escalates by name", unmarked_command_escalates),
        ("C-014 app/context.rs escalates (permit-list default)", cli_non_command_escalates),
        ("C-014 build.rs escalates ([security])", build_script_escalates),
        ("C-014 one red per live [security] glob", every_security_glob_escalates),
        ("C-014 self_update's update_check.rs escalates", self_update_escalates),
        ("C-014 an ocx_setup edit is scoped (control)", ocx_setup_is_scoped),
        ("C-014 a missing scoped_rows.toml exits 1", missing_rows_exit_1),
        ("C-014 a malformed scoped_rows.toml exits 1", malformed_rows_exit_1),
        ("C-013 an append to a pytestmark list is read", marker_append_is_read),
        ("C-013 [pytestmark, command] after a skipif reads the key only", marker_after_skipif_is_read),
        ("C-013 a multi-key marker selects on each key", multi_key_marker_selects_each),
        ("C-013 a non-literal marker is unreadable", non_literal_marker_is_unreadable),
        ("C-013 a command mark outside pytestmark is unreadable", stray_command_mark_is_unreadable),
        ("C-013 a carried-forward key and a two-mark list are read", marker_carried_forward_is_read),
        ("C-014 a security crate's Cargo.toml/README escalates, never routes", security_manifest_escalates),
        ("C-014 a changed crate with no [crates] row escalates", rowless_crate_escalates),
        ("C-014 an edit to test/scoped_rows.toml escalates", the_table_escalates_its_own_edit),
        ("a rename lists its removed path too", a_rename_lists_both_paths),
        ("test/lint/** routes to the lint tier", lint_tier_routes),
        ("C-015 coverage: the fixture tree is green", coverage_fixture_green),
        ("C-015 coverage: an unreached module reds (invariant 1)", coverage_red_module),
        ("C-015 coverage: an unnamed command file reds (invariant 2)", coverage_red_command),
        ("C-015 coverage: a hex.md security glob outside [security] reds (invariant 3)", coverage_red_security),
        ("C-015 coverage: an unparseable when: reds", coverage_red_unparseable_when),
        ("C-015 coverage: a module read below the floor reds", coverage_red_module_floor),
        ("C-015 coverage: command files read below the floor reds", coverage_red_command_floor),
        ("C-015 coverage: an unreadable marker reds naming the module", coverage_red_unreadable),
        ("C-015 coverage: a dead glob, a dangling key and a row mismatch red", coverage_red_table_drift),
        ("C-015 coverage: [security] never drops a REQUIRED_SECURITY glob", required_security_is_a_floor),
        ("C-015 coverage: a marker key wider than `<key>*` reds", wide_wildcard_key_reds),
        ("C-015 coverage: a [security] glob matching no tracked file reds", dead_security_glob_reds),
        ("a build script named by its manifest escalates", custom_build_script_escalates),
        ("C-015 coverage: escalate rows must equal TABLE_ESCALATES", escalate_rows_match_the_constant),
        ("C-015 coverage: a brace-less when: glob is read", a_single_when_glob_is_read),
        ("C-015 coverage: the live tree is green with its floors", coverage_live_green),
        ("crate plus route stays scoped", crates_and_routes_mix),
        ("empty change set is routed", nothing_changed_is_routed_with_nothing),
        ("escalation keeps crates informational", escalation_keeps_crates_informational),
        ("mark round trip and base selection", mark_round_trip),
        ("the verify mark lands where the hook reads it", mark_home_follows_the_project_dir),
        ("cargo metadata failure is loud", metadata_failure_is_loud),
        ("the manifest route reaches a workspace compile", the_manifest_route_reaches_a_workspace_compile),
        ("TABLE_ESCALATES matches test/scoped_rows.toml", table_rows_agree),
        ("C-017 AM-8 an unstaged edit under a merge refuses --mark", am8_unstaged_edit_under_a_merge_refuses),
        ("C-017 AM-8 an untracked file under a merge refuses --mark", am8_untracked_file_under_a_merge_refuses),
        ("C-017 AM-8 outside a merge an unstaged edit still marks (control)", am8_unstaged_edit_outside_a_merge_marks),
        ("C-017 AM-8 a clean paused merge marks the merged index tree", am8_a_clean_merge_marks_the_merged_tree),
        ("C-017 a clean verify outside a merge records its tree", a_clean_verify_outside_a_merge_records_its_tree),
        ("C-017 a tree dirtied during the run records no tree", a_tree_dirtied_during_the_run_records_no_tree),
        (
            "C-017 a pre-merge mark of a staged foreign tree admits no merge",
            a_premerge_mark_of_a_staged_foreign_tree_admits_no_merge,
        ),
        ("C-017 AM-8 --mark-precheck refuses a dirty merge up front", fix1_precheck_refuses_a_dirty_merge_up_front),
        ("C-017 AM-8 a tree staged during the run refuses the mark", fix1_a_tree_staged_during_the_run_refuses_the_mark),
        ("C-017 AM-8 a merge mark needs the precheck's snapshot", fix1_a_merge_mark_needs_the_precheck),
        ("C-017 AM-8 --mark-precheck outside a merge is a no-op", fix1_precheck_outside_a_merge_is_a_no_op),
        ("C-018 --log-run survives a plan failure", c018_log_run_survives_a_plan_failure),
        ("C-018 the run log lands in the common dir and never truncates", c018_run_log_lands_in_the_common_dir),
        ("C-018 append_jsonl unique_on skips a duplicate", c018_append_jsonl_unique_on_skips),
        ("C-018 a record on MERGE_HEAD^{tree} that missed the module is one escape", c018_a_missed_module_is_one_escape),
        ("C-018 a record on another tree is unattributed", c018_a_record_on_another_tree_is_unattributed),
        ("C-018 a record that selected the module is no escape", c018_a_record_that_selected_the_module_is_no_escape),
        ("C-018 no MERGE_HEAD is unattributed", c018_no_merge_head_is_unattributed),
        ("C-018 a run logged before staging matches on worktree_tree", c018_a_run_logged_before_staging_matches_on_worktree_tree),
        ("C-018 recording twice keeps one escape", c018_recording_twice_keeps_one_escape),
        ("C-018 a report older than --since is ignored", c018_a_report_older_than_since_is_ignored),
    ]


def self_test() -> int:
    scratch_root = REPO_ROOT / ".tmp"
    scratch_root.mkdir(exist_ok=True)
    failures = 0
    with tempfile.TemporaryDirectory(prefix="scoped-gate-", dir=scratch_root) as tmp:
        cases = _self_test_cases(Path(tmp))
        for name, thunk in cases:
            try:
                problem = thunk()
            except (SystemExit, NotImplementedError) as stop:
                problem = f"{type(stop).__name__}: {stop}"
            status = "ok  " if problem is None else "FAIL"
            failures += problem is not None
            print(f"{status} {name}")
            if problem is not None:
                print(f"       {problem}")
    print(f"self-test: {len(cases) - failures}/{len(cases)} rules hold ({failures} wrong)")
    return 1 if failures else 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--plan", action="store_true", help="print the gate plan as JSON")
    mode.add_argument(
        "--mark", choices=("full", "scoped"), help="write the verify mark at this scope"
    )
    mode.add_argument("--self-test", action="store_true", help="show every rule red/green")
    mode.add_argument(
        "--check-coverage",
        action="store_true",
        help="hold test/scoped_rows.toml and the command markers to the tree (C-015)",
    )
    mode.add_argument(
        "--mark-precheck",
        action="store_true",
        help="`task verify`'s first step: refuse a dirty merge now, snapshot its tree (C-017)",
    )
    mode.add_argument(
        "--record-escapes",
        action="store_true",
        help="attribute a failed T2 run's modules to the scoped runs (C-018)",
    )
    parser.add_argument("--crates", nargs="*", default=[], help="crates a scoped mark records")
    parser.add_argument(
        "--nocache", action="store_true", help="--mark: the run re-executed every acceptance module (NOCACHE=1)"
    )
    parser.add_argument("--junit-dir", type=Path, help="--record-escapes: the per-module JUnit copy")
    parser.add_argument(
        "--since", type=float, help="--record-escapes: ignore reports older than this epoch"
    )
    parser.add_argument(
        "--log-run",
        action="store_true",
        help="--mark scoped: append this run to the scoped-run log (verify:scoped only)",
    )
    ns = parser.parse_args(argv)
    if ns.self_test:
        return self_test()
    if ns.check_coverage:
        return check_coverage(REPO_ROOT)
    if ns.mark_precheck:
        refusal = mark_precheck(REPO_ROOT)
        if refusal:
            print(refusal, file=sys.stderr)
            return 1
        return 0
    if ns.record_escapes:
        if ns.junit_dir is None:
            parser.error("--record-escapes needs --junit-dir <dir>")
        return record_escapes(REPO_ROOT, ns.junit_dir, ns.since)
    if ns.mark:
        refusal = mark_refusal(REPO_ROOT)
        if refusal:
            print(refusal, file=sys.stderr)
            return 1
        head = git(REPO_ROOT, "rev-parse", "HEAD").strip()
        mark = write_mark(
            mark_file(), ns.mark, ns.crates, head, worktree_id(REPO_ROOT), proven_tree(REPO_ROOT), ns.nocache
        )
        print(f"verify mark: {json.dumps(mark, sort_keys=True)}")
        # One snapshot, one mark: a second `--mark` under the same merge must
        # come from a second verify, not reuse this run's evidence.
        start_snapshot(REPO_ROOT).unlink(missing_ok=True)
        if ns.log_run and ns.mark == "scoped":
            log_run(REPO_ROOT)
        return 0
    print(make_plan(REPO_ROOT, mark_file()).as_json())
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
