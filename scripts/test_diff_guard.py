#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Refuse a diff that changes what the acceptance suite asserts.

    scripts/test_diff_guard.py <base>..<head> [--allow test/tests/<file>.py:<line>]... [--tiered-shapes]

Proofs: scripts/tests/test_test_diff_guard.py

The crate split (plan_crate_split_workspace.md DEC-10, C-077, S-012) moves
code, never behaviour, and the acceptance suite is the proof: it must pass
unmodified at every commit. So under `test/` a merge may only

  - add `@pytest.mark.smoke` lines (and the `import pytest` that makes one
    resolve in a module that lacked it),
  - add `@pytest.mark.xdist_group("<name>")` lines — a scheduling constraint,
    the one mark that cannot change what a test asserts: it moves the test to
    a worker, and the tests it must not run beside are the reason. A group
    argument must be a literal, so the line names the slot it serialises on
    and a reader can grep for the other members,
  - change docstring / comment lines (path spellings follow the moved code),
  - add wholly new test functions (or test classes) to an existing module,
    or new `test/tests/**/*.py` modules — never a `conftest.py` or
    `__init__.py`, and never a `pytest.ini` / `tox.ini` / `setup.cfg`
    (pytest prefers any of those over `pyproject.toml`, so a new one
    silently replaces `--strict-markers`, `testpaths` and `pythonpath`),
  - change a `test/` file that a taskfile invokes — tooling the repo drives,
    not part of the pytest proof this guard freezes. `test/scripts/*.sh` is
    the case: `task test:index-conformance-drift` runs
    `sync_index_conformance.sh` and `verify-deep.yml` runs that task, so the
    "neither collected, imported nor run" rule below was false for it and
    forbade ever maintaining it. A file the collected suite *names* stays
    frozen even when a task also runs it, so this cannot thaw
    `test/docker-compose.yml`,
  - edit `test/pyproject.toml`, `test/taskfile.yml` and the floor files
    (`test/SUITE_FLOOR`, `test/SKIP_CEILING`, `test/XFAIL_CEILING`) — except
    that an added non-comment line there may not carry a pytest selection
    token (`--deselect`, `--ignore`, `--ignore-glob`, `-k`/`--keyword`,
    `-m`/`--markexpr`): those narrow the run without touching a test file.
    The one permitted expression is the smoke tier's own bare `-m smoke`
    (C-014, `test:smoke`) — `-m 'not smoke'`, `-m "smoke and x"` red.
    In a `.toml` among them the key check is a permit list too: an added
    line may set only a key of `_PERMITTED_CONFIG_KEYS`. Every way pytest
    has of collecting less is an ini key — `testpaths`, `norecursedirs`,
    `collect_ignore`, `collect_ignore_glob`, `python_files`, whatever the
    next release adds — so a list of the dangerous ones is a list already
    out of date, and a new key is a decision rather than an edit. The
    deletion direction has its own permit rule: a *removed* line may not
    drop a command-line option that no added line carries again. Removing
    a selection token widens the run and stays green; removing
    `--strict-markers` does not, because without it a misspelled
    `@pytest.mark.smok` stops being a collection error and becomes a test
    that silently leaves the `-m smoke` selection. A re-spelling that keeps
    every option passes.

Everything else is a violation and exits non-zero naming the line: a deleted
file, a removed code line, a changed line that carries `assert`,
`pytest.mark.skip`, `xfail`, `parametrize` or a fixture signature, an added
line of any other shape, an unchanged line that moved between code and a
docstring (a docstring opened one line and closed after the body turns every
assertion between into text while git reports the body as untouched), and any
edit to `test/src/**`, `test/conftest.py` or the other assets under `test/`.
One hunk may be allow-listed by `--allow
<path>:<line>` — the trampoline blob path constant at
`test/tests/test_trampoline_exec.py:852` (DEC-10 d) is the only intended use.
An allow-listed hunk must be exactly one line replacing one line: the
neighbours of that constant are asserts, and a `--unified=0` hunk spanning
them would let the allow-list re-point the constant and neutralise an assert
in one commit. Under `--tiered-shapes` an `--allow test/lint/<file>.py:<line>`
also names a reviewed rewrite of a module-level binding a moved test reads
(`_moved_global_problems`): every statement binding that name in the lint
home must be allow-listed at its first line, and what the rewrite reads is
held to the same rule in turn.

A wholly new test function or `Test*` class is exempt from the line checks
but not from every check — new code that rebinds what the existing tests
resolve makes their assertions vacuous with the diff reading as "added a
test". A new class must be `Test*` and hold only defs and a docstring; a new
def may carry only `@pytest.mark.*` decorators and no default argument
(evaluated at import); a new class may carry no decorator, base or
keyword.

The body rule is DEC-10(b) as ruled 2026-09-16, a permit rule: a new def
(and every def in a new module, and a new module's own import-time
statements — module level and class bodies alike, where `monkeypatch`
does not exist and nothing is undone at teardown) stores only into names
bound in that same scope, and `monkeypatch` — the fixture parameter — is
the only sanctioned mutation route. Concretely (`_store_problems`): every
store target (assignment, augmented or annotated assignment, walrus,
`del`, `for` and `with` targets) must root in a parameter or a name the
scope bound earlier to a fresh value — a name bound to a reference into shared state
(`c = helpers.CACHE`, `x = REGISTRY`) stays shared, and a container built
here is the scope's own while what it holds is not, so reaching back
through it (`box = [os.environ]`, `box[0]["X"] = …`) reaches what was put
in. An attribute call is a permit rule too (`_PURE_READS`): on a receiver
the scope does not own — a module, a module-level object, a container
reached back through — only a listed pure read may be called, so
`helpers.CACHE.sort()`, `os.putenv(…)`, `shutil.rmtree(…)` and
`Path("test/pytest.ini").write_text(…)` red beside the mutators that were
once enumerated, while `re.search(…)`, `ast.parse(…)`, `p.read_text()` and
a call into the suite's own frozen `test/src/**` (`helpers.make_package()`)
pass. A binding is followed through an alias, so `change =
os.environ.update` then `change({…})` is the same refusal at the bare-name
call. On a *fixture parameter* the rule stays the mutator enumeration
(`_MUTATORS`), because calling a fixture's methods is what a test does:
`ocx.run(…)` and `tmp_path.joinpath(…)` pass, `ocx.env.update(…)` does
not. `monkeypatch` — the fixture parameter — is exempt from both
(`monkeypatch.setattr(helpers, …)`, `monkeypatch.setitem(REGISTRY, …)`,
`with monkeypatch.context() as m` pass), and `os.environ["X"] = …`,
`REGISTRY["k"] = …`, `sys.path.append(…)` and `COUNT += 1` do not;
`global` and `nonlocal` are refused outright. On
top of that (`_rebinding`, every walk): the name `getattr`, `setattr`,
`delattr`, `globals`, `vars`, `exec`, `eval`, `__import__`,
`__builtins__`, `MonkeyPatch` or `pytest_plugins` in any position (bare,
aliased, subscripted); an attribute that is a dunder other than
`__name__`; a `.MonkeyPatch(`, `.patch(`, `.start(`, `.import_module(`,
`.reload(`, `.exec(` or `.eval(` call (each persists past the test or
executes text); an assignment or walrus aliasing an imported module
(`h = helpers`); a `sys.modules` reference; a class keyword; and, at
module level of a new module, a store into an imported module's attribute
or item — imports bind wherever they appear, def-local ones included.

A new `test/tests/**/*.py` module is not accepted by name alone: the same
walk runs over its whole body, module level and every def (it may hold
helpers, constants and module-local fixtures — those change nothing outside
it — but not a store into, or a mutating call on, anything it imports). The plan's own structural tests
under `test/tests/` (PLAN_OWNED_STRUCTURAL_TESTS) are exempt from the line
checks — they are the plan's oracles (C-014, C-048), edited by later WPs,
and never an acceptance assertion DEC-10 protects — and get that whole-module
walk instead. Every other module, in-series acceptance tests included, gets
the full checks — so the guard is run over both the per-WP range and the
merge-base range at each merge (DX-17), and neither range hides an edit the
other would show.

`--tiered-shapes` (plan_test_speed_tiers.md C-007, P-2) admits five more
shapes, and only when passed — without it each is refused exactly as above
(DEC-10's default, S-023): (a) a `test*` def removed under `test/tests/`
whose AST-identical twin is added under `test/lint/` (a whole module when
every def is matched; statements the move orphans may go with it); (b) a new
`test/lint/conftest.py`, `test/lint/test_*.py`, `test/LINT_FLOOR` or a
`test/LINT_*_CEILING`, whose
own code is walked as new code except what it carries verbatim from a source
module of its moved tests — module-level statements and undecorated helper
defs; (c) one
`pytest.mark.command("<key>", ...)` statement per module, in the form its
existing `pytestmark` binding dictates; (d) `test/scoped_rows.toml` and
`test/LINT_FLOOR` and the `test/LINT_*_CEILING` pair as config; (e) a removed def named by an added
`// ported-from:` marker whose Rust test executed and passed in a fresh
`target/bazel/junit.xml` — the guard runs `task bazel:test:unit` itself at
HEAD (AM-7). A `test/SUITE_FLOOR` decrease may then not exceed the collected
count of the (a)/(e) removals, and `test/LINT_FLOOR` may not decrease at all. In a config file, a `cargo` line's `--release`
counts as the `--profile` it abbreviates (C-021's `--profile test-bin`).

Stdlib only; driven by `git diff`. Its proofs (`scripts/tests/test_test_diff_guard.py`)
build one throwaway git repository per shape under pytest's own `tmp_path` and
show the guard red on every forbidden shape and green on every allowed one —
a guard that was never seen red is a habit, not a check.
"""
from __future__ import annotations

import argparse
import ast
import io
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tokenize
import xml.etree.ElementTree as ET
from collections.abc import Callable
from dataclasses import dataclass, field, replace
from pathlib import Path

from _git import git

REPO_ROOT = Path(__file__).resolve().parents[1]

# DEC-10 (c): these may change freely — short of an added selection token.
ALLOWED_CONFIG = frozenset(
    {
        "test/pyproject.toml",
        "test/taskfile.yml",
        "test/SUITE_FLOOR",
        "test/SKIP_CEILING",
        "test/XFAIL_CEILING",
        # The Bazel arm of the same suite (C-024). Both carry the acceptance
        # target list, so an edit to either can narrow what runs — which is why
        # they are judged here rather than exempted.
        "test/BUILD.bazel",
        "test/bazel.bzl",
    }
)
# pytest's ways of running fewer tests, or none, from a config or task line:
# the long spellings; `-k` / `-m` starting an option word (the lookbehind is
# what keeps `--maxfail` and `--markexpr`'s own dashes and a bundled `-n`
# out) — with no lookahead, because argparse takes an attached value and
# `-kfoo` selects exactly as `-k foo` does; `-o key=` (`set -o pipefail` has
# no `=`); `-c <file>.ini|.toml|.cfg` (`sh -c '…'` has no config suffix);
# and the ini keys that pick what is collected. Every spelling here was
# probed green before it was added.
_SELECTION_TOKEN = re.compile(
    r"--(?:deselect|ignore|ignore-glob|keyword|markexpr|co|collect-only|override-ini|noconftest"
    r"|lf|last-failed|ff|failed-first|sw|stepwise)\b"
    r"|(?<![\w-])-[km]"
    r"|(?<![\w-])-o[\s=]*['\"]?\w+="
    r"|(?<![\w-])-c[\s=]*['\"]?\S+\.(?:ini|toml|cfg)\b"
)
# The two permitted spellings: the smoke tier's own bare `-m smoke` (C-014,
# `test:smoke`; `smoke` as a whole word, not followed by a boolean operator —
# `smoke and x` narrows it) and the floor tasks' `--collect-only -q`, whose
# output is read as a count. Both landed in the series; refusing them would
# red the merge-base range for its remainder.
_PERMITTED = re.compile(
    r"(?:-m|--markexpr)[\s=]*['\"`]?smoke\b(?![\w\s]*\b(?:and|or)\b)|--collect-only -q\b"
)


# The keys an added line of a `.toml` in ALLOWED_CONFIG may carry — every key
# `test/pyproject.toml` holds today, minus `testpaths`. Inverted on purpose:
# the enumeration of collection-narrowing keys this replaced was probed green
# and still leaked `norecursedirs`, `collect_ignore` and `collect_ignore_glob`,
# and pytest is free to add another. Each key here narrows nothing silently —
# `markers`, `dev` and `requires-python` fail loudly when they shrink, the
# retention keys touch only tmpdir reuse, and `addopts`' narrowing spellings
# are caught as tokens above. A new key is a decision, so it reds until it is
# listed here.
_PERMITTED_CONFIG_KEYS = frozenset(
    {
        "name",
        "version",
        "requires-python",
        "pythonpath",
        "addopts",
        "markers",
        "tmp_path_retention_count",
        "tmp_path_retention_policy",
        "dev",
    }
)
# Callables that return a grip on something outside the scope rather than a
# value built from their arguments — the binding then carries the provenance
# of what was opened (`handle_target`). Everything else a call returns is
# fresh, which is what keeps `dict(os.environ)` and `os.environ.copy()` the
# mutable copies an acceptance test builds a subprocess environment from.
_HANDLE_FACTORIES = frozenset({"open", "Path", "PurePath", "PosixPath", "WindowsPath", "NamedTemporaryFile"})
# The provenance of a handle opened on a literal: a name no scope can bind,
# so the binding is a reference into something this scope does not own.
_OUTSIDE = "<outside this scope>"
_CONFIG_KEY = re.compile(r"^\s*(?P<key>[A-Za-z_][\w.-]*)\s*=")
# A command-line option on a config or task line, for the deletion direction
# of the permit rule (`check_config_edit`). A dash must start a word and be
# followed by a letter, so a YAML list item (`- task: lint`) and a go-task
# heredoc marker (`>-`) are not options; a value is cut at `=`
# (`--basetemp={{.X}}` and `--basetemp=/other` are the same option).
_OPTION = re.compile(r"(?<![\w-])(--?[A-Za-z][\w-]*)")


def _options(text: str) -> set[str]:
    """Every command-line option an added or removed line carries; comments are inert."""
    return set() if text.lstrip().startswith("#") else set(_OPTION.findall(text))


def _tiered_options(text: str) -> set[str]:
    """`_options`, with cargo's `--release` read as the `--profile` it abbreviates.

    `--tiered-shapes` only (plan_test_speed_tiers.md C-021 builds the acceptance
    binary under `--profile test-bin`). The deletion rule compares option
    *names* — a value is cut, so `--profile release` -> `--profile test-bin`
    already passes — and cargo documents `--release` as `--profile release`.
    Read that way, the short spelling is held exactly as the long one is: the
    profile may change, every other option on the line must survive. Only on a
    line that invokes `cargo`; anywhere else `--release` stays its own option.
    """
    options = _options(text)
    if "--release" in options and "cargo" in text.split():
        options = (options - {"--release"}) | {"--profile"}
    return options


def _replacement(removed: str, hunk: Hunk, hunks: list[Hunk], path: str) -> str | None:
    """The single added line `removed` turned into, or None if it just went.

    A `.toml` setting is identified by its key and may move anywhere in the
    file, so there the candidate is the added line with the same key. Anything
    else is matched inside its own `--unified=0` hunk — the contiguous
    replacement — by word overlap, which picks the re-spelling of the same
    command over a neighbour that merely mentions the option: an edit turning
    `uv run pytest --strict-markers -q` into `uv run pytest -q` pairs with the
    pytest line, not with an `echo` added beside it. Ties go to the first,
    and a candidate sharing no word at all is not a replacement.
    """
    if path.endswith(".toml") and (m := _CONFIG_KEY.match(removed)):
        same_key = [
            text
            for h in hunks
            for _, text in h.added
            if (a := _CONFIG_KEY.match(text)) and a["key"] == m["key"]
        ]
        return same_key[0] if same_key else None
    words = set(removed.split())
    scored = [(len(words & set(text.split())), index, text) for index, (_, text) in enumerate(hunk.added)]
    best = max(scored, default=(0, 0, ""), key=lambda row: (row[0], -row[1]))
    return best[2] if best[0] else None


def _selection_hit(text: str, path: str = "") -> str | None:
    """Why an added config line narrows the run, or None; comments are inert.

    A pytest selection token in any ALLOWED_CONFIG file, and — in a `.toml`,
    where a key is an ini setting rather than a shell assignment — a key
    outside `_PERMITTED_CONFIG_KEYS`.
    """
    if text.lstrip().startswith("#"):
        return None
    for m in _SELECTION_TOKEN.finditer(text):
        if not _PERMITTED.match(text, m.start()):
            return f"the pytest selection token `{m.group(0)}`"
    # Scoped to the one pytest config: a `test/scoped_rows.toml` key
    # (`ocx_setup = [...]`, C-007(d)) names a crate, not an ini setting.
    if path == "test/pyproject.toml" and (m := _CONFIG_KEY.match(text)) and m["key"] not in _PERMITTED_CONFIG_KEYS:
        return f"the config key `{m['key']}`"
    return None

# The crate-split plan's own structural / oracle tests (plan C-014: smoke
# coverage over the visible verbs and the deep-tier triggers; C-048: the
# logging observables and the no-crate-path scan). They are written and
# revised by the plan's WPs, assert nothing about ocx's behaviour that
# existed at v0.6.2, and so are outside DEC-10's "no assertion change" — the
# line checks skip them. Nothing else is: an in-series acceptance test such
# as test_patch_smoke.py is an acceptance test like any other.
PLAN_OWNED_STRUCTURAL_TESTS = frozenset(
    {
        "test/tests/test_smoke_coverage.py",
        "test/tests/test_logging.py",
        "test/tests/test_no_crate_path_assertions.py",
        # Their lint-tier homes for the move window (C-007, C-LINT): the same
        # oracles, so the same whole-module walk wherever they live.
        "test/lint/test_smoke_coverage.py",
        "test/lint/test_logging.py",
        "test/lint/test_no_crate_path_assertions.py",
    }
)


def split_range(spec: str) -> tuple[str, str]:
    """`A..B` → (A, B); `A...B` → (merge-base, B) once resolved by the caller."""
    if "..." in spec:
        base, head = spec.split("...", 1)
        return base, head or "HEAD"
    if ".." in spec:
        base, head = spec.split("..", 1)
        return base, head or "HEAD"
    raise SystemExit(f"expected <base>..<head>, got {spec!r}")


# ---------------------------------------------------------------------------
# the guard
# ---------------------------------------------------------------------------


FORBIDDEN_KEYWORDS = (
    "assert",
    "pytest.mark.skip",
    "xfail",
    "parametrize",
    "pytest.fixture",
    "@fixture",
)
SIGNATURE = re.compile(r"^\s*(async\s+)?def\s")
MARKER_LINE = "@pytest.mark.smoke"
IMPORT_LINE = "import pytest"
#: `@pytest.mark.xdist_group("slot")` — scheduling only. Pinning a test to a
#: worker cannot change what it asserts, and a shared registry-wide resource
#: (the reserved `global` patch-descriptor repository) is serialised no other
#: way. The group must be a string LITERAL: a name built at runtime hides which
#: slot is being joined, and the point of the line is that it is greppable.
XDIST_GROUP_LINE = re.compile(r'^@pytest\.mark\.xdist_group\("[A-Za-z0-9_]+"\)$')


def _keyword_hit(text: str) -> str | None:
    for word in FORBIDDEN_KEYWORDS:
        if word in text:
            return word
    return "a fixture signature" if SIGNATURE.match(text) else None


def _fixture_only(mode: str) -> bool:
    """`test/src/**` and `test/conftest.py` are fixture semantics; never editable."""
    return mode.startswith("test/src/") or mode == "test/conftest.py"


# Names pytest reads for configuration or collection semantics rather than as
# a test module: a new one under `test/` changes how every existing test runs.
_NEVER_NEW = frozenset({"conftest.py", "__init__.py", "pytest.ini", "tox.ini", "setup.cfg"})

# The Bazel files the adoption adds under `test/`: the acceptance package and
# the macro declaring its 172 `sh_test` targets (WP-36), the cast package and
# its macro (WP-33). Enumerated rather than allowed by shape, so a further
# Bazel file under `test/` is still a decision - a new package there moves the
# boundary that `//:` labels resolve against, which has twice taken `//...`
# down in this series.
_NEW_BUILD_FILES = frozenset(
    {
        "test/BUILD.bazel",
        "test/bazel.bzl",
        "test/doc_scripts/BUILD.bazel",
        "test/doc_scripts/cast.bzl",
    }
)

# BEP fixtures for `scripts/bep_to_otlp.py`'s self-test. They are captured
# Bazel output, read by a gate and by nothing pytest collects, so they change
# no existing test's behaviour - the property this guard defends.
_NEW_FIXTURE_PREFIX = "test/fixtures/bep/"


def _new_file_allowed(path: str) -> bool:
    """Only a new `test/tests/**/*.py` module — the (b) of DEC-10 — may appear.

    Everything else is refused by shape rather than by an enumeration of
    what is dangerous: a `test/pytest.ini` beats `pyproject.toml` in pytest's
    config search and can drop `--strict-markers` or `--deselect` a test, a
    `test/src/**` module is fixture semantics, a `test/tests/__init__.py`
    turns the tree into a package and changes rootdir-relative module names.
    """
    return (
        path in _NEW_BUILD_FILES
        or (path.startswith(_NEW_FIXTURE_PREFIX) and path.endswith(".json"))
    ) or (
        path.startswith("test/tests/")
        and path.endswith(".py")
        and Path(path).name not in _NEVER_NEW
    )


@dataclass(slots=True)
class Hunk:
    old_start: int
    old_len: int
    new_start: int
    new_len: int
    removed: list[tuple[int, str]] = field(default_factory=list)
    added: list[tuple[int, str]] = field(default_factory=list)

    def covers(self, line: int) -> bool:
        in_old = self.old_start <= line < self.old_start + max(self.old_len, 1)
        in_new = self.new_start <= line < self.new_start + max(self.new_len, 1)
        return in_old or in_new


_HUNK_HEADER = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def parse_hunks(diff: str) -> list[Hunk]:
    """Removed and added lines per hunk of a `--unified=0` diff.

    `---` / `+++` are file headers only between a `diff ` line and the first
    `@@`; inside a hunk they are content — a removed `--x` or an added `++x`
    line — and skipping them there would drop exactly the lines that look
    least like a docstring edit.
    """
    hunks: list[Hunk] = []
    old = new = 0
    in_hunk = False
    for line in diff.splitlines():
        header = _HUNK_HEADER.match(line)
        if header:
            o, ol, n, nl = header.groups()
            hunks.append(Hunk(int(o), int(ol or 1), int(n), int(nl or 1)))
            old, new = int(o), int(n)
            in_hunk = True
            continue
        if line.startswith("diff "):
            in_hunk = False
            continue
        if not in_hunk or line.startswith("\\ "):
            continue
        if line.startswith("-"):
            hunks[-1].removed.append((old, line[1:]))
            old += 1
        elif line.startswith("+"):
            hunks[-1].added.append((new, line[1:]))
            new += 1
    return hunks


def inert_lines(source: str) -> set[int]:
    """Line numbers a change may touch freely: docstrings, comment-only, blank.

    Docstrings by `ast` (the first statement of a module, class or function
    when it is a string), comment-only and blank lines by `tokenize` — so a
    blank line inside a multi-line string constant is NOT inert (it is part
    of a STRING token, not an NL token), and a trailing comment on a code
    line does not make that line inert.

    Nor does a docstring: a `; return` appended to one keeps `body[0]` a
    string constant while the line now also carries a statement that
    neuters every assertion under it. So any line a statement other than
    the docstrings themselves starts on is code, whatever else shares it.
    """
    inert: set[int] = set()
    tree = ast.parse(source)
    docstrings: set[int] = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            body = node.body
            if (
                body
                and isinstance(body[0], ast.Expr)
                and isinstance(body[0].value, ast.Constant)
                and isinstance(body[0].value.value, str)
            ):
                docstrings.add(id(body[0]))
                inert.update(range(body[0].lineno, (body[0].end_lineno or body[0].lineno) + 1))
    inert -= {
        node.lineno for node in ast.walk(tree) if isinstance(node, ast.stmt) and id(node) not in docstrings
    }
    lines = source.splitlines()
    for tok in tokenize.generate_tokens(io.StringIO(source).readline):
        row, col = tok.start
        if row > len(lines):
            continue
        comment_only = tok.type == tokenize.COMMENT and lines[row - 1][:col].strip() == ""
        blank = tok.type == tokenize.NL and lines[row - 1].strip() == ""
        if comment_only or blank:
            inert.add(row)
    return inert


def unchanged_pairs(hunks: list[Hunk], old_total: int, new_total: int) -> list[tuple[int, int]]:
    """(old line, new line) for every line the diff reports as untouched.

    A diff is exactly the claim that the old file minus its removed lines
    reads the same as the new file minus its added lines, so pairing those
    two remainders in order is the old→new map — no second diff needed.
    `hunks` must be every hunk of the file, allow-listed ones included.
    """
    removed = {n for h in hunks for n, _ in h.removed}
    added = {n for h in hunks for n, _ in h.added}
    pairs: list[tuple[int, int]] = []
    old = new = 1
    while old <= old_total and new <= new_total:
        if old in removed:
            old += 1
        elif new in added:
            new += 1
        else:
            pairs.append((old, new))
            old += 1
            new += 1
    return pairs


def _qualified_defs(source: str) -> dict[str, ast.AST]:
    """`Class.method` / `func` → node, for every def and class in `source`."""
    out: dict[str, ast.AST] = {}

    def walk(nodes: list[ast.stmt], prefix: str) -> None:
        for node in nodes:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                name = f"{prefix}{node.name}"
                out[name] = node
                if isinstance(node, ast.ClassDef):
                    walk(node.body, f"{name}.")

    walk(ast.parse(source).body, "")
    return out


def _span(node: ast.AST) -> range:
    first = min([node.lineno, *(d.lineno for d in getattr(node, "decorator_list", []))])
    return range(first, (node.end_lineno or node.lineno) + 1)


def _is_fixture_or_hook(node: ast.AST) -> bool:
    if isinstance(node, ast.ClassDef):
        return False
    if node.name.startswith("pytest_"):
        return True
    return any("fixture" in ast.unparse(d) for d in getattr(node, "decorator_list", []))


# Names that reach a namespace or a module by value, refused in any context —
# bare, aliased (`g = globals`), or subscripted (`__builtins__["exec"]`).
# `pytest_plugins` is a plugin load, a conftest in disguise; `MonkeyPatch`
# constructs an instance nothing undoes.
_REFUSED_NAMES = frozenset(
    {
        "getattr",
        "setattr",
        "delattr",
        "globals",
        "vars",
        "exec",
        "eval",
        "__import__",
        "__builtins__",
        "MonkeyPatch",
        "pytest_plugins",
        # `exec` under another spelling: unpickling constructs whatever the
        # payload names, so `pickle.loads(b"…")` runs arbitrary code with the
        # diff reading as "parsed a fixture". Refused by module rather than
        # by verb, so that `loads` stays a pure read for `json` and `tomllib`.
        "pickle",
        "marshal",
        "shelve",
        "dill",
    }
)
# Method names whose call persists past the test or executes text, on any
# receiver: `pytest.MonkeyPatch()`, `mock.patch(...)` / `.start()`,
# `importlib.import_module` / `.reload`, `builtins.exec`.
_REFUSED_METHODS = frozenset(
    {"__setattr__", "__delattr__", "MonkeyPatch", "start", "import_module", "reload", "exec", "eval", "patch"}
)
# Method names that mutate their receiver, refused on a *fixture parameter*
# (`ocx.env.update(…)`): there the enumeration stays, because a test's whole
# idiom is calling methods on what pytest handed it (`ocx.run`,
# `tmp_path.joinpath`) and a permit list would have to enumerate every
# fixture's API instead. On a receiver the scope does not own at all — a
# module, a module-level object, a container reached back through — the rule
# is `_PURE_READS` below, a permit list.
_MUTATORS = frozenset(
    {
        "update",
        "append",
        "extend",
        "insert",
        "pop",
        "popitem",
        "clear",
        "setdefault",
        "remove",
        "discard",
        "add",
        "__setitem__",
        "__delitem__",
        "setattr",
        "delattr",
        "register",
    }
)
# The attribute calls a new test may make on a receiver this scope does not
# own. Inverted for the reason `_PERMITTED_CONFIG_KEYS` is: the enumeration
# of mutators this replaced was probed green and still leaked
# `Path("test/pytest.ini").write_text(…)`, `helpers.CACHE.sort()`,
# `os.putenv(…)` and `shutil.rmtree(…)` — every standard library is free to
# spell a side effect with a verb nobody listed, so a list of the dangerous
# ones is a list already out of date. Each name here only reads its receiver,
# so a call cannot outlive the test; a verb that is not here is a decision,
# and reds until it is added.
#
# Absent on purpose, each because one plausible receiver spells a side
# effect that outlives the test, and the receiver's type is not knowable
# here. The way round each is to derive the value from a call, which binds
# a fresh object this scope owns, or to read by subscript — neither is
# judged. `replace`: `str.replace` reads, `Path.replace` renames a file.
# `dump`: `json.dump(obj, fh)` / `yaml.dump(obj, fh)` take a stream and
# write to it (`dumps` returns a string and stays; `yaml.safe_dump`'s
# `stream=None` second parameter is the same hazard, so it is not here
# either). `load`: `json.load(fh)` needs a handle this scope opened and
# `pickle.load` runs arbitrary code — `read_text()` plus `loads` is the
# shape with neither. `copy`: `dict.copy()` reads, `shutil.copy(src, dst)`
# and `Path.copy(target)` (3.14) write a file. `get`: `dict.get` reads,
# `requests.get` / `session.get` reach the network, against the suite's
# offline posture — `mapping["k"]` is a subscript and passes. `importorskip`:
# `pytest.importorskip("x")` imports, with every import-time side effect and
# a `sys.modules` entry, which is the ground `.import_module(` is refused on.
_PURE_READS = frozenset(
    {
        # str / bytes
        "startswith", "endswith", "strip", "lstrip", "rstrip", "removeprefix", "removesuffix",
        "split", "rsplit", "splitlines", "partition", "rpartition", "join", "format",
        "lower", "upper", "title", "casefold", "capitalize", "encode", "decode",
        "count", "index", "find", "rfind", "isdigit", "isidentifier", "isupper", "islower",
        "zfill", "ljust", "rjust", "expandtabs",
        # mapping / set / sequence reads
        "keys", "values", "items", "union", "intersection", "difference",
        "issubset", "issuperset", "most_common", "elements",
        # re
        "compile", "escape", "match", "fullmatch", "search", "sub", "subn",
        # (`.start(` is not here: `_REFUSED_METHODS` refuses it on every
        # receiver, a `mock.patch(…).start()` nothing stops.)
        "findall", "finditer", "group", "groups", "groupdict", "end", "span",
        # ast / inspect / json / yaml / tomllib — parse and render, never store
        "parse", "unparse", "walk", "iter_child_nodes", "iter_fields", "get_docstring",
        "dumps", "loads", "safe_load", "getsource", "signature",
        # pathlib reads
        "read_text", "read_bytes", "resolve", "absolute", "glob", "rglob", "iterdir",
        "exists", "is_file", "is_dir", "is_symlink", "stat", "lstat", "samefile",
        "joinpath", "relative_to", "with_suffix", "with_name", "with_stem", "as_posix",
        "as_uri", "expanduser", "home", "cwd", "readlink",
        # pytest's own read-only surface
        "raises", "warns", "approx", "param",
    }
)


def _rebinding(node: ast.AST, imports: dict[str, str]) -> str | None:
    """Why `node` can rebind what the existing tests resolve, or None.

    One predicate for every walk (a new def in an existing module, a new
    module, a plan-owned module) so the three cannot drift apart; the
    scope-aware store rule of DEC-10(b) is `_def_problems`, run beside it
    over every def. Reads through an import (`helpers.make_package()`,
    `sys.argv[0]`, `helpers.__name__`) pass: a Store/Del context, a module
    alias, a dunder other than `__name__`, a refused name or method, or a
    class keyword is what reds here.
    """
    if isinstance(node, ast.Global):
        return f"declares `global {', '.join(node.names)}`"
    if isinstance(node, ast.Name) and (node.id in _REFUSED_NAMES or imports.get(node.id) in _REFUSED_NAMES):
        # Through the import, not through the spelling: `import pickle as pk`
        # and `from pickle import loads` are the same module under two other
        # names, and an alias must not be a way past this set.
        source = imports.get(node.id)
        return f"names `{node.id}`" if node.id in _REFUSED_NAMES else f"names `{node.id}`, which is `{source}`"
    if isinstance(node, ast.Attribute):
        if node.attr.startswith("__") and node.attr.endswith("__") and node.attr != "__name__":
            return f"reaches a dunder: `{ast.unparse(node)}`"
        if node.attr == "modules" and _root_name(node.value) == "sys":
            return "touches `sys.modules`"
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr in _REFUSED_METHODS:
        return f"calls `.{node.func.attr}(`"
    if (
        isinstance(node, (ast.Assign, ast.AnnAssign, ast.NamedExpr))
        and isinstance(node.value, ast.Name)
        and node.value.id in imports
    ):
        return f"aliases the imported module `{node.value.id}`"
    if isinstance(node, ast.ClassDef) and node.keywords:
        return f"class `{node.name}` carries a keyword (metaclass) — evaluated at import"
    if (
        isinstance(node, (ast.Attribute, ast.Subscript))
        and isinstance(node.ctx, (ast.Store, ast.Del))
        and _root_name(node.value) in imports
    ):
        return f"stores to an imported module's attribute or item: `{ast.unparse(node)}`"
    return None


def _receiver_root(node: ast.expr) -> str | None:
    """The Name a receiver chain roots in, through attributes, items and calls."""
    while True:
        if isinstance(node, (ast.Attribute, ast.Subscript)):
            node = node.value
        elif isinstance(node, ast.Call):
            node = node.func
        else:
            return node.id if isinstance(node, ast.Name) else None


def _store_problems(
    body: list[ast.stmt], scope: dict[str, str], *, enter_defs: bool, suite: frozenset[str] = frozenset()
) -> list[str]:
    """DEC-10(b) strict (see the module docstring): stores and mutating calls
    in `body` root only in names bound within it; `monkeypatch` is the one
    sanctioned mutation route.

    `scope` maps a name to its kind — "own" (bound to a fresh value: a
    call's result, a literal, a loop item), "param", or "monkeypatch". A
    bare-Name binding whose value is a reference into something outside
    the scope (`c = helpers.CACHE`, `x = REGISTRY`) removes the name from
    the scope instead: it is that shared object under another name. Visits
    in source order, value before target, so "bound earlier" is literal.

    `enter_defs=False` stops at each def, for a module's or a class's own
    import-time statements: `module_rebinding_problems` walks every def
    separately with its parameters in scope, and a def body judged under the
    module's scope would read `monkeypatch` as a stranger.

    `suite` names the modules imported from the acceptance suite's own `src`
    package. This guard refuses every edit to `test/src/**` (`_fixture_only`),
    so that API is frozen: `helpers.make_package()` is a read of something
    that cannot change. Every other module is judged by `_PURE_READS`.
    """
    problems: list[str] = []
    # name → what it aliases, for a mutation laundered through a local name:
    # `change = os.environ.update` then `change({…})` is a bare-Name call and
    # reaches no attribute-call rule unless the binding is remembered.
    aliases: dict[str, tuple[str, str | None, str]] = {}
    # name → the kind of a reference a display literal it was bound to holds.
    # The container itself is fresh (`todo = [ocx]` may be appended to), but
    # reaching back through it (`box = [os.environ]`, `box[0]["X"] = …`) is a
    # reach into what was put in.
    held: dict[str, str | None] = {}

    def reference_root(value: ast.expr | None) -> str | None:
        # Attributes and items of a name are references into it; a call's
        # result is a fresh object, whatever it was called on — except a
        # handle factory, which hands back a grip on something outside.
        while isinstance(value, (ast.Attribute, ast.Subscript)):
            value = value.value
        if isinstance(value, ast.Call) and (opened := handle_target(value)) is not None:
            return _OUTSIDE if isinstance(opened, str) else reference_root(opened) or _OUTSIDE
        return value.id if isinstance(value, ast.Name) else None

    def element_of(value: ast.expr | None) -> ast.expr | None:
        """What an unpacked name or a loop item is bound to.

        A display literal hands out its own items, so the one that makes the
        binding shared is the first element that is itself a reference
        (`for c in [helpers.CACHE]`); anything else hands out references
        into itself (`a, b = helpers.PAIR`). Binding the literal itself is
        not this — `todo = [fn]` is a fresh list whatever it holds.
        """
        if isinstance(value, (ast.Tuple, ast.List, ast.Set)):
            return next((elt for elt in value.elts if reference_root(elt) is not None), None)
        return value

    def handle_target(call: ast.Call) -> ast.expr | str | None:
        """What a call hands back a grip on, or None when its result is fresh.

        `Path(p)` and `open(p)` return a handle into whatever `p` names, and
        a method on such a handle (`Path(p).resolve()`, `p.parent`) is still
        a handle on it — so `p = Path("test/pytest.ini")` then
        `p.write_text(…)` is the one-liner R7 named, one line longer, and is
        judged the same way. A literal argument names something outside this
        scope entirely, so it gets `_OUTSIDE`.

        Every other call stays fresh: `dict(os.environ)` and `_helper(ocx)`
        build a new object out of their arguments, and `os.environ.copy()`
        and `re.search(…)` are ordinary method calls on a receiver this rule
        never reached — which is what keeps a copied environment mutable.
        """
        while True:
            if isinstance(call.func, ast.Name):
                if call.func.id not in _HANDLE_FACTORIES:
                    return None
                return call.args[0] if call.args else _OUTSIDE
            if not isinstance(call.func, ast.Attribute):
                return None
            inner = call.func.value
            while isinstance(inner, (ast.Attribute, ast.Subscript)):
                inner = inner.value
            if not isinstance(inner, ast.Call):
                return None
            call = inner

    def contained_root(value: ast.expr | None) -> str | None:
        """The root of a reference a display literal holds, or None.

        `[os.environ]` and `{"env": os.environ}` hand out `os.environ` at the
        first subscript; a nested display is the same one level further in.
        """
        if isinstance(value, ast.Dict):
            elts: list[ast.expr | None] = list(value.values)
        elif isinstance(value, (ast.Tuple, ast.List, ast.Set)):
            elts = list(value.elts)
        else:
            return None
        for elt in elts:
            if elt is not None and (root := reference_root(elt) or contained_root(elt)) is not None:
                return root
        return None

    def receiver_kind(receiver: ast.expr) -> tuple[str | None, str]:
        """(`own` / `monkeypatch` / `param` / `suite` / None, the receiver as text).

        None is "this scope does not own it": an import, a module-level
        object, a name rebound to a reference into one, or a reach back
        through a container into what it holds.
        """
        root = _receiver_root(receiver)
        text = ast.unparse(receiver)
        if root is None:
            return "own", text  # an expression receiver — a literal, a BinOp: fresh
        if isinstance(receiver, ast.Name):
            return ("suite" if root in suite else scope.get(root)), text
        if root in held:
            return held[root], text
        return scope.get(root), text

    def judge_call(kind: str | None, text: str, attr: str, lineno: int) -> None:
        """The one mutation rule for every attribute call (see `_PURE_READS`)."""
        if kind in ("own", "monkeypatch", "suite"):
            return
        if kind == "param":
            if attr in _MUTATORS:
                problems.append(
                    f"line {lineno}: mutates `{text}` via `.{attr}(` — not a name bound "
                    "in this scope and not `monkeypatch` (DEC-10(b))"
                )
            return
        if attr not in _PURE_READS:
            problems.append(
                f"line {lineno}: calls `.{attr}(` on `{text}` — a receiver this scope does not own, "
                f"and `{attr}` is not one of the permitted pure reads (DEC-10(b))"
            )

    def refuse(target: ast.expr) -> None:
        problems.append(
            f"line {target.lineno}: stores into `{ast.unparse(target)}` — not a name bound in this scope (DEC-10(b))"
        )

    def bind(target: ast.expr, value: ast.expr | None) -> None:
        """A fresh binding of `target` to `value` (None: an item, a `del`-able local)."""
        if isinstance(target, ast.Name):
            aliases.pop(target.id, None)
            held.pop(target.id, None)
            if isinstance(value, ast.Attribute):
                kind, text = receiver_kind(value.value)
                if kind not in ("own", "monkeypatch", "suite"):
                    aliases[target.id] = (value.attr, kind, text)
            if (inner := contained_root(value)) is not None:
                held[target.id] = scope.get(inner)
            root = reference_root(value)
            if root is None:
                scope[target.id] = "own"
            elif root in scope:
                scope[target.id] = scope[root]
            else:
                scope.pop(target.id, None)
        elif isinstance(target, (ast.Tuple, ast.List)):
            # Unpacking binds each name to an element, not to a fresh object:
            # `a, b = helpers.PAIR` binds two references into `helpers`. Two
            # displays of equal length pair off; anything else hands every
            # name the same element.
            paired = value.elts if isinstance(value, (ast.Tuple, ast.List)) and len(value.elts) == len(target.elts) else None
            for index, elt in enumerate(target.elts):
                bind(elt, paired[index] if paired else element_of(value))
        elif isinstance(target, ast.Starred):
            bind(target.value, element_of(value))
        elif _receiver_root(target) not in scope or _receiver_root(target) in held:
            # `in held`: a store through a container reaches what the
            # container holds, not the container — `box = [os.environ]`
            # then `box[0]["OCX_OFFLINE"] = "1"`.
            refuse(target)

    def rebind(target: ast.expr) -> None:
        """`x += …` / `del x`: not a binding — `x` must already be the def's own."""
        if _receiver_root(target) not in scope or (
            not isinstance(target, ast.Name) and _receiver_root(target) in held
        ):
            refuse(target)

    def visit(node: ast.AST) -> None:
        match node:
            case ast.Global() | ast.Nonlocal():
                problems.append(f"line {node.lineno}: declares `{type(node).__name__.lower()} {', '.join(node.names)}`")
                return
            case ast.Assign(targets=targets, value=value):
                visit(value)
                for t in targets:
                    bind(t, value)
                return
            case ast.AugAssign(target=target, value=value):
                visit(value)
                rebind(target)
                return
            case ast.AnnAssign(target=target, value=value):
                if value is not None:
                    visit(value)
                bind(target, value)
                return
            case ast.NamedExpr(target=target, value=value):
                visit(value)
                bind(target, value)
                return
            case ast.Delete(targets=targets):
                for t in targets:
                    rebind(t)
                return
            case ast.For(target=target, iter=it) | ast.AsyncFor(target=target, iter=it) | ast.comprehension(target=target, iter=it):
                visit(it)
                # A loop item comes out of the iterable, so it carries the
                # iterable's provenance for the same reason unpacking does.
                bind(target, element_of(it))
            case ast.withitem(context_expr=expr, optional_vars=var):
                visit(expr)
                if var is not None:
                    bind(var, expr)
                return
            case ast.ExceptHandler(name=str() as name):
                scope[name] = "own"
            case ast.FunctionDef(name=name) | ast.AsyncFunctionDef(name=name):
                scope[name] = "own"
                if not enter_defs:
                    return
            case ast.ClassDef(name=name):
                # A class body runs at import, in its own namespace: walked
                # like the module's, with the class name bound.
                scope[name] = "own"
            case ast.Lambda(args=largs):
                for a in [*largs.posonlyargs, *largs.args, *largs.kwonlyargs, largs.vararg, largs.kwarg]:
                    if a is not None:
                        scope[a.arg] = "own"
            case ast.Call(func=ast.Attribute(value=receiver, attr=attr)):
                judge_call(*receiver_kind(receiver), attr, node.lineno)
            case ast.Call(func=ast.Name(id=name)) if name in aliases:
                # The same rule through the alias the binding recorded:
                # `change = os.environ.update` then `change({…})`.
                attr, kind, text = aliases[name]
                judge_call(kind, text, attr, node.lineno)
        for child in ast.iter_child_nodes(node):
            visit(child)

    for stmt in body:
        visit(stmt)
    return problems


def _def_problems(
    fn: ast.FunctionDef | ast.AsyncFunctionDef, suite: frozenset[str] = frozenset()
) -> list[str]:
    """`_store_problems` over a def's body, with its parameters in scope."""
    args = fn.args
    return _store_problems(
        fn.body,
        {
            a.arg: "monkeypatch" if a.arg == "monkeypatch" else "param"
            for a in [*args.posonlyargs, *args.args, *args.kwonlyargs, args.vararg, args.kwarg]
            if a is not None
        },
        enter_defs=True,
        suite=suite,
    )


def module_rebinding_problems(source: str) -> list[str]:
    """`line N: <why>` for every node of a whole module `_rebinding` refuses,
    the module's own import-time stores and mutating calls, and every def's
    DEC-10(b) problems."""
    imports = _import_bindings(source)
    suite = _suite_modules(source)
    tree = ast.parse(source)
    problems = [f"line {node.lineno}: {why}" for node in ast.walk(tree) if (why := _rebinding(node, imports))]
    # Import time is the scope `_def_problems` cannot reach: the very
    # `os.environ.update(...)` refused inside a def runs here at collection,
    # before any test does, and binds nothing back.
    problems += _store_problems(tree.body, {}, enter_defs=False, suite=suite)
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            problems += _def_problems(node, suite)
    return problems


def _import_bindings(source: str) -> dict[str, str]:
    """Every name an `import` / `from … import` binds in `source` → its source package.

    Def-local imports included. The value is the top-level package the name
    came out of, so `_rebinding` can judge what a name *is* rather than what
    it is spelled: `import pickle as pk` and `from pickle import loads` bind
    `pk` and `loads`, and both resolve to `pickle`. Membership still works as
    a set (`name in imports`), which is how the alias and store rules read it.
    """
    names: dict[str, str] = {}
    for node in ast.walk(ast.parse(source)):
        if isinstance(node, ast.Import):
            for alias in node.names:
                names[alias.asname or alias.name.split(".")[0]] = alias.name.split(".")[0]
        elif isinstance(node, ast.ImportFrom):
            package = (node.module or "").split(".")[0]
            for alias in node.names:
                names[alias.asname or alias.name] = package
    return names


def _suite_modules(source: str) -> frozenset[str]:
    """Names bound to a module of the acceptance suite's own `src` package.

    `from src import helpers`, `import src.helpers as h`. This guard refuses
    every edit to `test/src/**` (`_fixture_only`), so what those modules do
    is frozen: `helpers.make_package()` is a read of an API that cannot
    change under the series. Every other module's calls are judged by
    `_PURE_READS` — `os.putenv(…)` and `helpers.make_package()` are the same
    shape and only the module tells them apart.
    """
    names: set[str] = set()
    for node in ast.walk(ast.parse(source)):
        if isinstance(node, ast.Import):
            names.update(
                alias.asname or alias.name.split(".")[0]
                for alias in node.names
                if alias.name == "src" or alias.name.startswith("src.")
            )
        elif isinstance(node, ast.ImportFrom) and (node.module or "").split(".")[0] == "src":
            names.update(alias.asname or alias.name for alias in node.names)
    return frozenset(names)


def _is_pytest_mark(decorator: ast.expr) -> bool:
    """`@pytest.mark.<x>` or `@pytest.mark.<x>(…)` — nothing else."""
    if isinstance(decorator, ast.Call):
        decorator = decorator.func
    return (
        isinstance(decorator, ast.Attribute)
        and isinstance(decorator.value, ast.Attribute)
        and isinstance(decorator.value.value, ast.Name)
        and decorator.value.value.id == "pytest"
        and decorator.value.attr == "mark"
    )


def _root_name(node: ast.expr) -> str | None:
    while isinstance(node, (ast.Attribute, ast.Subscript)):
        node = node.value
    return node.id if isinstance(node, ast.Name) else None


def _new_def_problem(
    node: ast.FunctionDef | ast.AsyncFunctionDef, imports: dict[str, str], suite: frozenset[str]
) -> str | None:
    """Why a wholly new def is not just a test, or None (see the module docstring)."""
    foreign = [ast.unparse(d) for d in node.decorator_list if not _is_pytest_mark(d)]
    if foreign:
        return f"carries a decorator other than @pytest.mark.*: {foreign}"
    if node.args.defaults or any(d is not None for d in node.args.kw_defaults):
        return "has a default argument (evaluated at import time)"
    if why := next((why for inner in ast.walk(node) if (why := _rebinding(inner, imports))), None):
        return why
    return next(iter(_def_problems(node, suite)), None)


def _new_class_problem(node: ast.ClassDef) -> str | None:
    if not node.name.startswith("Test"):
        return "is a class that is not `Test*`"
    if node.decorator_list or node.bases or node.keywords:
        # None of the three is walked for a new class, and a Test* class
        # needs none: each is evaluated at import.
        return "is a class carrying a decorator, base or keyword"
    for index, item in enumerate(node.body):
        docstring = (
            index == 0
            and isinstance(item, ast.Expr)
            and isinstance(item.value, ast.Constant)
            and isinstance(item.value.value, str)
        )
        if not (docstring or isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))):
            return f"is a class whose body holds more than defs and a docstring (line {item.lineno})"
    return None


def new_definition_lines(base_src: str, head_src: str, added: set[int]) -> tuple[set[int], list[str]]:
    """Lines belonging to wholly new test functions / classes, plus the problems.

    A definition is wholly new when every line of its span (decorators
    included) was added. It is allowed only if it is a test (`test*`) or a
    `Test*` class, its qualified name does not already exist in the base file
    (a later same-named def would silently replace the original), it is
    neither a fixture nor a `pytest_*` hook (either changes how the existing
    tests in that module run), and it is shaped like a test — the class body
    holds only defs, the def carries only `@pytest.mark.*`, no default
    argument, and a body that cannot rebind a module global
    (`_new_def_problem`): anything that could would make an existing
    assertion vacuous with the guard reading "new test".
    """
    base_names = set(_qualified_defs(base_src))
    imports = _import_bindings(head_src)
    suite = _suite_modules(head_src)
    exempt: set[int] = set()
    problems: list[str] = []
    for qualname, node in _qualified_defs(head_src).items():
        span = _span(node)
        if not set(span) <= added:
            continue
        if qualname in base_names:
            problems.append(f"line {span.start}: new `{qualname}` shadows a definition the base file already has")
        elif _is_fixture_or_hook(node):
            problems.append(f"line {span.start}: new `{qualname}` is a fixture or pytest hook — it changes how the existing tests run")
        elif isinstance(node, ast.ClassDef):
            if why := _new_class_problem(node):
                problems.append(f"line {span.start}: new `{qualname}` {why}")
        elif not node.name.startswith("test"):
            problems.append(f"line {span.start}: new `{qualname}` is not a test function — helpers belong in a new file")
        elif why := _new_def_problem(node, imports, suite):
            problems.append(f"line {span.start}: new `{qualname}` {why} — a new test may not rebind what the existing tests resolve")
        exempt.update(span)
    return exempt, problems


def check_python_edit(
    repo: Path,
    base: str,
    head: str,
    path: str,
    allow: frozenset[tuple[str, int]],
    inert_only: bool = False,
    *,
    sanctioned: frozenset[str] = frozenset(),
    markers: bool = False,
) -> list[str]:
    """Judge a `test/**` edit line by line; `inert_only` drops the additive routes.

    `inert_only` is for the modules the collected tests *import* — prose may
    change, nothing else. A new `def`, a smoke marker and an `import pytest`
    are all meaningful additions to a collected test module and meaningless in
    a fixture module, where the same shapes would be new fixture behaviour
    arriving under the permit written for new tests.

    `sanctioned` and `markers` are `--tiered-shapes` only (C-007): the test
    defs whose removal (a)/(e) already sanctioned, and whether the one
    `pytest.mark.command` statement of shape (c) is admitted.
    """
    base_src = git(repo, "show", f"{base}:{path}")
    head_src = git(repo, "show", f"{head}:{path}")
    diff = git(repo, "diff", "--no-color", "--no-renames", "--unified=0", base, head, "--", path)
    all_hunks = parse_hunks(diff)
    problems: list[str] = []
    hunks: list[Hunk] = []
    for hunk in all_hunks:
        allowed_at = [line for p, line in allow if p == path and hunk.covers(line)]
        if allowed_at and (hunk.old_len, hunk.new_len) == (1, 1):
            continue
        if allowed_at:
            # A `--unified=0` hunk grows over adjacent changed lines, so an
            # allow-list on one line would carry its changed neighbours —
            # asserts, at the trampoline constant. Only a one-for-one line
            # re-point is what the allow-list means; the hunk is then also
            # judged line by line below.
            problems.append(
                f"{path}:{allowed_at[0]}: allow-listed hunk replaces {hunk.old_len} line(s) with "
                f"{hunk.new_len} — only a single-line re-point may be allow-listed"
            )
        hunks.append(hunk)
    base_inert = inert_lines(base_src)
    head_inert = inert_lines(head_src)
    added_all = {n for h in hunks for n, _ in h.added}
    exempt, def_problems = new_definition_lines(base_src, head_src, added_all)
    if inert_only:
        exempt = frozenset()
    problems += [f"{path}:{p}" for p in def_problems]
    removed_exempt: set[int] = set()
    if sanctioned:
        removed_all = {n for h in hunks for n, _ in h.removed}
        removed_exempt = _sanctioned_base_lines(base_src, head_src, sanctioned, removed_all)
    if markers:
        marker_exempt, marker_problems = _marker_statement(base_src, head_src, hunks)
        exempt = set(exempt) | marker_exempt
        problems += [f"{path}:{p}" for p in marker_problems]
    # Inertness is judged per side, and git never shows an unchanged line — so
    # a docstring opened at one line and closed after the body is two inert
    # edits around a body that silently became text. Every untouched line
    # must be code on both sides or text on both sides.
    head_lines = head_src.splitlines()
    for old_no, new_no in unchanged_pairs(all_hunks, len(base_src.splitlines()), len(head_lines)):
        if (old_no in base_inert) != (new_no in head_inert):
            problems.append(
                f"{path}:{new_no}: line moved between code and docstring without a diff: "
                f"{head_lines[new_no - 1].strip()!r}"
            )
    for hunk in hunks:
        for lineno, text in hunk.removed:
            if lineno in removed_exempt:
                continue
            if word := _keyword_hit(text):
                problems.append(f"{path}:-{lineno}: removed line carries {word}: {text.strip()!r}")
            elif lineno not in base_inert:
                problems.append(f"{path}:-{lineno}: removed line is not a docstring/comment: {text.strip()!r}")
        for lineno, text in hunk.added:
            if lineno in exempt:
                continue
            stripped = text.strip()
            if not inert_only and (
                stripped in (MARKER_LINE, IMPORT_LINE) or XDIST_GROUP_LINE.match(stripped)
            ):
                continue
            if word := _keyword_hit(text):
                problems.append(f"{path}:+{lineno}: changed line carries {word}: {stripped!r}")
            elif lineno not in head_inert:
                problems.append(
                    f"{path}:+{lineno}: added line is not `{MARKER_LINE}`, "
                    f'`@pytest.mark.xdist_group("…")`, `{IMPORT_LINE}`, '
                    f"a docstring/comment or a new test: {stripped!r}"
                )
    return problems


#: The reader floor for `_named_by_the_suite`, as a *name* rather than a count.
#: A count calibrated to one tree is re-tuned by the next extraction; this file
#: exists in every tree the guard can be pointed at, so its absence means the
#: scan failed rather than that the suite shrank.
_SUITE_READER_WITNESS = "test/conftest.py"


def _unambiguous_needle(path: str, under_test: list[str]) -> str:
    """The shortest right-anchored path suffix that names `path` and nothing else.

    A bare basename was the first spelling and it is constant-True for any
    shared name: ten files under `test/` are called `README.md`, so the needle
    `README.md` is found in some reader no matter which README is being judged,
    and the predicate returns the same answer in every state. That is the very
    defect this route was added to fix, reintroduced one level down.

    Lengthening unconditionally is not the fix either — `conftest.py` names
    `docker-compose.yml` bare, so requiring a parent segment would make the
    compose file read as unnamed and reopen the hole. The discriminator is
    *ambiguity*, not length: take the shortest suffix that identifies one file.
    """
    segments = path.split("/")
    for start in range(len(segments) - 1, -1, -1):
        suffix = "/".join(segments[start:])
        matches = sum(1 for p in under_test if p == suffix or p.endswith("/" + suffix))
        if matches == 1:
            return suffix
    return path


def _named_by_the_suite(repo: Path, head: str, path: str) -> bool | None:
    """Does anything pytest collects or imports name this asset?

    `None` means the reader found nothing to read, which must not be allowed to
    read as "nothing names it" — that is DEC-52's shape one level up, and here
    it would silently free the whole frozen set.
    """
    under_test = git(repo, "ls-tree", "-r", "--name-only", head, "--", "test").split()
    readers = [
        p for p in under_test
        if p.endswith(".py")
        and (p.startswith(("test/tests/", "test/src/")) or p == "test/conftest.py")
    ]
    if _SUITE_READER_WITNESS not in readers:
        return None
    needle = _unambiguous_needle(path, under_test)
    return any(needle in git(repo, "show", f"{head}:{p}") for p in readers)


#: The reader floor for `_invoked_by_a_taskfile`, a *name* for the same reason
#: `_SUITE_READER_WITNESS` is one: this file exists in every tree that has an
#: acceptance suite at all, so its absence means the scan failed.
_TASKFILE_WITNESS = "test/taskfile.yml"

#: go-task keys whose value is prose, not a command. Their values are removed
#: before the search, and that removal is the difference between a signal and a
#: bypass: `test/taskfile.yml` is an ALLOWED_CONFIG file, so anyone may add a
#: line to it — and if a `desc:` counted, writing `desc: syncs fixtures/x.json`
#: would free `fixtures/x.json` from this guard in the same permitted edit.
_TASKFILE_PROSE_KEYS = frozenset({"desc", "summary", "msg"})


def _command_text(source: str) -> str:
    """`source` with comments and the values of `_TASKFILE_PROSE_KEYS` removed.

    A deliberately small reader, and the point is which way it is wrong. It
    keeps every line that is not prose, so a file merely *mentioned* by a
    `dir:` or an `env:` reads as invoked — a false positive whose only
    consequence is that a `test/` asset becomes maintainable, which is a
    reviewable line in a taskfile. It drops text it cannot classify (an inline
    ` #` inside a command), so a genuine invocation can read as absent — a
    false negative that leaves the old, stricter classification in place. Both
    errors point away from freeing something silently.

    What it therefore proves: *some taskfile names this file outside prose*.
    What it does not prove: that the naming line is a `cmd:`, that the task is
    reachable from a gate, or that it ever runs. The claim in the branch below
    is scoped to match.
    """
    kept: list[str] = []
    prose_indent: int | None = None
    for line in source.splitlines():
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())
        if prose_indent is not None:
            # A block scalar's body is indented past its key; the first line at
            # or left of the key ends it.
            if stripped and indent <= prose_indent:
                prose_indent = None
            else:
                continue
        if stripped.startswith("#"):
            continue
        key, sep, _ = stripped.partition(":")
        if sep and key.lstrip("- ") in _TASKFILE_PROSE_KEYS:
            prose_indent = indent
            continue
        kept.append(line.split(" #", 1)[0])
    return "\n".join(kept)


def _invoked_by_a_taskfile(repo: Path, head: str, path: str) -> bool | None:
    """Does a taskfile name this `test/` file outside prose?

    `None` means the reader found nothing to read. It is returned rather than
    `False` because the two are the same answer with opposite meanings: `False`
    reads as "no task runs this", which is the over-strict classification this
    route exists to correct, arriving with no message to say the scan broke.

    Two floors, both fail-loud. The first is `_TASKFILE_WITNESS`: no taskfile
    in the tree, no judgement. The second is that the scan found *some* file
    under `test/` named by some taskfile — a reader that stopped matching (a
    needle rule that drifted, a taskfile grammar this misses) would otherwise
    answer `False` for every path and be indistinguishable from a repository
    whose tasks genuinely run nothing under `test/`.
    """
    tracked = git(repo, "ls-tree", "-r", "--name-only", head).split()
    taskfiles = [
        p for p in tracked
        if p == "taskfile.yml" or p.endswith("/taskfile.yml")
        or (p.startswith("taskfiles/") and p.endswith((".yml", ".yaml")))
    ]
    if _TASKFILE_WITNESS not in taskfiles:
        return None
    commands = "\n".join(_command_text(git(repo, "show", f"{head}:{p}")) for p in taskfiles)
    under_test = [p for p in tracked if p == "test" or p.startswith("test/")]
    if _unambiguous_needle(path, under_test) in commands:
        return True
    if not any(_unambiguous_needle(p, under_test) in commands for p in under_test):
        return None
    return False


def check_relocation_only(
    repo: Path, base: str, head: str, path: str, allow: frozenset[tuple[str, int]]
) -> list[str]:
    """A `test/` asset the suite neither collects, imports, nor runs.

    `test/scripts/*.sh` is the case: a vendoring helper that asserts nothing
    and that no test invokes. Freezing it protects no property of the suite,
    but it names crate paths, so the split moves the tree under it — and the
    `else` that used to catch it never consulted `--allow`, which made it the
    one shape in this guard with no clearing move at all. It now takes exactly
    the DEC-10 (d) permit and nothing more: a hunk that replaces one line with
    one line, named on the command line. Any other hunk reds as before.
    """
    # Markdown is documentation by construction: pytest cannot import it, no
    # fixture executes it, and nothing asserts a `.md` without naming it from
    # code that would itself be frozen. The naming test is the wrong instrument
    # here — `simplesigning/README.md` is named only by a docstring
    # cross-reference in `cosign_artifacts.py`, which reads the README's
    # *siblings*. Judging documentation by who mentions it makes the suite
    # "run" its own prose.
    #
    # Deliberately NOT done by asking whether the mention was inert: the real
    # `test/conftest.py` names `docker-compose.yml` on inert lines only (its
    # code lines say "docker-compose" without the extension), so an
    # inertness test frees the compose file and reopens exactly the hole DEC-69
    # closed. Measured, after shipping that mistake into this function.
    #
    # A `.md` still cannot be rewritten freely: the relocation route below
    # permits one allow-listed one-for-one line, which is a reviewed action.
    if not path.endswith(".md"):
        # Which side of "nor runs" this file falls on is derived, never listed.
    # `test/docker-compose.yml` is named by `conftest.py`, `helpers.py` and two
    # collected modules — the session fixture starts it, so the suite runs it
    # as surely as it runs a test, and DEC-65's route let it through because
    # the route keyed on "not Python" instead of on what the suite does with
    # the file. `sync_index_conformance.sh` is named by nothing collected or
    # imported, which is what makes it an asset rather than a fixture.
        named = _named_by_the_suite(repo, head, path)
        if named is None:
            return [
                (
                    f"{path}: refusing to judge — the suite-reader scan did not find "
                    f"{_SUITE_READER_WITNESS}, so 'named by nothing' and 'read nothing' are "
                    "indistinguishable here"
                )
            ]
        if named:
            return [
                (
                    f"{path}: a module pytest collects or imports names this file, so the suite "
                    "runs it — frozen outright, exactly like the modules that name it"
                )
            ]
        # Checked only after `named`, and the order is the rule: a file the
        # collected suite names stays frozen even when a task also runs it, so
        # `test/docker-compose.yml` cannot be freed by the task that starts the
        # registry. DEC-69's hole stays closed.
        #
        # Below that, "neither collected, imported nor run" was simply false
        # for `test/scripts/*.sh`: `task test:index-conformance-drift` executes
        # `sync_index_conformance.sh` and `verify-deep.yml` runs that task, so
        # the relocation-only rule — calibrated for an inert data asset that
        # merely carries crate paths the split moves — forbade ever maintaining
        # a live script. The fix for a days-old CI red arrived as ten hunks and
        # could not land; `--allow` is line-scoped and one-for-one, so it
        # cannot express them.
        #
        # The claim this branch makes is exactly what `_command_text` supports:
        # a taskfile names this file outside prose, so it is tooling the repo
        # drives rather than part of the pytest proof — the proof being what
        # this guard freezes. It is NOT a claim that the task is on a gate.
        invoked = _invoked_by_a_taskfile(repo, head, path)
        if invoked is None:
            return [
                (
                    f"{path}: refusing to judge — the taskfile scan found no {_TASKFILE_WITNESS} "
                    "or no invocation of anything under test/, so 'no task runs it' and 'the scan "
                    "read nothing' are indistinguishable here"
                )
            ]
        if invoked:
            return []

    diff = git(repo, "diff", "--no-color", "--no-renames", "--unified=0", base, head, "--", path)
    problems: list[str] = []
    hunks = parse_hunks(diff)
    if not hunks:
        # A chmod produces a `M` row and no hunks, so every loop below is a
        # no-op and the file passes having been judged by nothing.
        return [f"{path}: changed with no line hunks — a mode-only change is still a change"]
    for hunk in hunks:
        allowed_at = [line for p, line in allow if p == path and hunk.covers(line)]
        if allowed_at and (hunk.old_len, hunk.new_len) == (1, 1):
            continue
        problems.append(
            f"{path}:{hunk.new_start}: only an allow-listed one-for-one line re-point may change "
            f"in a test/ asset that is neither collected, imported nor run "
            f"({hunk.old_len} line(s) -> {hunk.new_len})"
        )
    return problems


def check_config_edit(repo: Path, base: str, head: str, path: str, tiered: bool = False) -> list[str]:
    """What an edit to an ALLOWED_CONFIG file may not do, in both directions.

    Added lines: a pytest selection token or an unlisted `.toml` key — the
    permit rule of `_selection_hit`, which narrows what is collected.

    Removed lines: an option the file carried and no added line carries
    again. The deletion direction needs its own rule rather than
    `_selection_hit` over `hunk.removed`, because *removing* a selection
    token widens the run and is correctly green — what is not green is
    dropping a hardening option, and `--strict-markers` is the case: without
    it a misspelled `@pytest.mark.smok` stops being a collection error and
    becomes a test that silently leaves the `-m smoke` selection. So the
    permit rule here is "an option survives the edit": a re-spelling that
    keeps every option passes, a deletion reds naming the option.

    `tiered` (`--tiered-shapes`) reads cargo's `--release` as `--profile`
    (`_tiered_options`); without it the comparison is today's, byte for byte.
    """
    diff = git(repo, "diff", "--no-color", "--no-renames", "--unified=0", base, head, "--", path)
    hunks = parse_hunks(diff)
    problems = [
        f"{path}:+{lineno}: added line carries {token}: {text.strip()!r}"
        for hunk in hunks
        for lineno, text in hunk.added
        if (token := _selection_hit(text, path))
    ]
    # The ONE line that replaced this one, never a bag of tokens from
    # elsewhere: pooled over the file, `version = "--strict-markers"` in the
    # same `pyproject.toml` launders the deletion, and pooled over the hunk,
    # an adjacent `echo 'uses -p no:randomly'` does. `_replacement` picks the
    # single added line the removed one turned into.
    for hunk in hunks:
        for lineno, text in hunk.removed:
            options = _tiered_options if tiered else _options
            kept = options(_replacement(text, hunk, hunks, path) or "")
            for opt in sorted(options(text) - kept):
                problems.append(
                    f"{path}:-{lineno}: removed line drops the option `{opt}`, which the line that "
                    f"replaced it does not carry: {text.strip()!r}"
                )
    return problems


def _module_problems(repo: Path, head: str, path: str) -> list[str]:
    return [f"{path}:{p}" for p in module_rebinding_problems(git(repo, "show", f"{head}:{path}"))]


# ---------------------------------------------------------------------------
# --tiered-shapes (plan_test_speed_tiers.md C-007, P-2): opt-in, never default
# ---------------------------------------------------------------------------

# (d) The config files the tiered plan adds. Joined to ALLOWED_CONFIG only
# under the flag, so without it they are refused as today's new non-test file.
LINT_FLOOR = "test/LINT_FLOOR"
TIERED_CONFIG = frozenset(
    {"test/scoped_rows.toml", LINT_FLOOR, "test/LINT_SKIP_CEILING", "test/LINT_XFAIL_CEILING"}
)
LINT_DIR = "test/lint/"
# (e) The per-case report `task bazel:test:unit` writes — one `testcase` per
# `#[test]`, `classname` the target, `name` the libtest path (AM-7). Never
# `bazel-testlogs/**/test.xml`, which holds one `testcase` per *target*.
UNIT_JUNIT = "target/bazel/junit.xml"
#: `// ported-from: test/tests/<module>.py::<case>` on a line of its own (C-RUBRIC);
#: `<case>` in pytest's spelling, `Class::method` for a method.
PORTED_FROM = re.compile(
    r"^\s*//\s*ported-from:\s*(?P<module>test/tests/[\w/.-]+\.py)::(?P<case>[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*$"
)

#: Runs the unit lane in `repo`'s working tree and returns its exit status.
UnitRunner = Callable[[Path], int]
#: `(repo, base, nodeids)` → the count `pytest --collect-only -q` reports for
#: `nodeids` in the `base` tree.
Collector = Callable[[Path, str, list[str]], int]


def run_unit_tests(repo: Path) -> int:
    """`task bazel:test:unit` in `repo` — the only producer of UNIT_JUNIT.

    No `sources:` cache on that task, so it always rewrites the report; a
    green Bazel graph replays its test results from cache, which is what makes
    running it here affordable.
    """
    task = shutil.which("task")
    if task is None:
        print("test-diff guard: `task` is not on PATH — run the guard under `ocx exec --`", file=sys.stderr)
        return 127
    # The lane's output is progress, not this guard's verdict: stdout stays the problem list.
    return subprocess.run([task, "bazel:test:unit"], cwd=repo, stdout=sys.stderr, check=False).returncode


_COLLECTED = re.compile(r"^(\d+) tests? collected", re.MULTILINE)


def collect_count(repo: Path, base: str, nodeids: list[str]) -> int:
    """One `pytest --collect-only -q` over `nodeids` at `base`, OCX_TESTS_NO_REGISTRY=1.

    The base's whole tree is exported to a scratch directory and collected
    there with the checkout's own pinned environment (`uv run --project
    test`) — a parametrized test counts every case, which is the unit
    `SUITE_FLOOR` is in. The whole tree, not `test/`: a module may import from
    anywhere in the repository (`website/scripts` onto `sys.path`) and
    parametrize over any of it (doc anchors, crate sources), and a partial
    export turns the first into an ImportError and the second into a
    different count than the base tree has.
    """
    scratch_root = repo / ".tmp"
    scratch_root.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="diff-guard-collect-", dir=scratch_root) as tmp:
        archive = subprocess.run(["git", "-C", str(repo), "archive", base], capture_output=True, check=False)
        if archive.returncode != 0:
            raise SystemExit(f"test-diff guard: git archive {base} failed:\n{archive.stderr.decode(errors='replace')}")
        with tarfile.open(fileobj=io.BytesIO(archive.stdout)) as tar:
            tar.extractall(tmp, filter="data")
        result = subprocess.run(
            ["uv", "run", "--project", str(REPO_ROOT / "test"), "--directory", str(Path(tmp) / "test"),
             "pytest", "--collect-only", "-q", "--color=no", *nodeids],
            capture_output=True, text=True, encoding="utf-8", check=False,
            env=os.environ | {"OCX_TESTS_NO_REGISTRY": "1"},
        )
    counted = _COLLECTED.findall(result.stdout)
    if result.returncode != 0 or not counted:
        raise SystemExit(
            f"test-diff guard: collecting the {len(nodeids)} removed test(s) at {base} failed "
            f"(exit {result.returncode}):\n{result.stdout[-2000:]}{result.stderr[-2000:]}"
        )
    count = int(counted[-1])
    # Reader floor: every removed def collects at least one case, so fewer means
    # pytest read something other than what was asked.
    if count < len(nodeids):
        raise SystemExit(f"test-diff guard: {count} case(s) collected for {len(nodeids)} removed test def(s) at {base}")
    return count


@dataclass(slots=True)
class Tiered:
    """The `--tiered-shapes` options; the two callables are the self-test's seams."""

    unit_runner: UnitRunner = run_unit_tests
    collector: Collector = collect_count


def _lint_file_allowed(path: str) -> bool:
    """(b) `test/lint/conftest.py` and `test/lint/test_*.py`, direct children only.

    (`test/LINT_FLOOR` and the `test/LINT_*_CEILING` pair are admitted as
    TIERED_CONFIG, where their edits are judged.)
    `__init__.py` would make `test/lint` a package and change rootdir-relative
    module names; a data file or a subdirectory is a decision, not a lint test.
    """
    rel = path.removeprefix(LINT_DIR)
    return path.startswith(LINT_DIR) and "/" not in rel and (
        rel == "conftest.py" or (rel.startswith("test_") and rel.endswith(".py"))
    )


def _removed_test_defs(base_src: str, head_src: str | None) -> dict[str, ast.AST]:
    """Qualname → base node for every `test*` def the head no longer has (all, if deleted)."""
    kept = set(_qualified_defs(head_src)) if head_src is not None else set()
    return {
        qualname: node
        for qualname, node in _qualified_defs(base_src).items()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name.startswith("test")
        and qualname not in kept
    }


def _move_key(defs: dict[str, ast.AST], qualname: str) -> tuple[str, str]:
    """(qualname, the def as written plus every enclosing class's own context).

    `ast.dump` omits positions by default, so the def is compared exactly as
    written — decorators, docstring and body — wherever it sits. A method also
    carries each enclosing class's decorators, bases, keywords and non-def
    body: `@pytest.mark.usefixtures("check") class TestX` moved to a bare
    `class TestX` runs its method without the fixture, and the method alone
    would read identical.
    """
    parts = qualname.split(".")
    context = []
    for depth in range(1, len(parts)):
        cls = defs[".".join(parts[:depth])]
        if isinstance(cls, ast.ClassDef):
            own = [item for item in cls.body if not isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))]
            context.append(ast.dump(ast.Module(body=[*cls.decorator_list, *cls.bases, *cls.keywords, *own], type_ignores=[])))
    return qualname, "\n".join([*context, ast.dump(defs[qualname])])


def _added_lint_defs(
    repo: Path, base: str, head: str, listing: list[tuple[str, str]]
) -> dict[tuple[str, str], list[str]]:
    """`_move_key` of every def new under `test/lint/test_*.py` → the lint paths holding one.

    One path per copy, because one added def sanctions one removal: two
    identical tests in two modules read different module constants, and one
    lint copy is one of them — the path is where `_moved_global_problems`
    reads the constants the copy runs against.
    """
    found: dict[tuple[str, str], list[str]] = {}
    for status, path in listing:
        if status not in ("A", "M") or not _lint_file_allowed(path) or not Path(path).name.startswith("test_"):
            continue
        existing = set(_qualified_defs(git(repo, "show", f"{base}:{path}"))) if status == "M" else set()
        defs = _qualified_defs(git(repo, "show", f"{head}:{path}"))
        for qualname, node in defs.items():
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and qualname not in existing:
                found.setdefault(_move_key(defs, qualname), []).append(path)
    return found


# A moved def is identical only together with the module-level names it runs
# against: `for value in CASES: assert value` reads the same with `CASES = []`
# as with `CASES = [False]`, and only one of them fails. So every name a moved
# def reads — transitively, through the helpers, fixtures and constants that
# bind it — must be bound in the lint home by the same statements as in the
# source. The one sanctioned difference is a path constant (C-007(a): "path
# constants move"), judged by `_path_constant`.
_SCOPES = (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Lambda)
_PATH_FACTORIES = frozenset({"Path", "PurePath", "PosixPath", "PurePosixPath"})
_PATH_METHODS = frozenset({"resolve", "absolute", "joinpath", "with_name", "with_suffix", "expanduser"})


def _chain_root(node: ast.expr) -> str | None:
    """The name an attribute/subscript chain hangs off (`A.b[0].c` → `A`), calls not followed."""
    while isinstance(node, (ast.Attribute, ast.Subscript)):
        node = node.value
    return node.id if isinstance(node, ast.Name) else None


def _bound_names(stmt: ast.stmt) -> set[str]:
    """The module-level names ``stmt`` binds or mutates.

    A def/class binds its name; an import its aliases (`*` for a star import);
    anything else every name stored, deleted or augmented at module scope —
    including through an attribute or subscript (`CASES[0] = …`) and a
    top-level mutating call (`CASES.append(…)`). Nested scopes are not entered.
    """
    if isinstance(stmt, _SCOPES):
        return {stmt.name}
    if isinstance(stmt, ast.Import):
        return {alias.asname or alias.name.split(".")[0] for alias in stmt.names}
    if isinstance(stmt, ast.ImportFrom):
        return {alias.asname or alias.name for alias in stmt.names}
    names: set[str] = set()
    todo: list[ast.AST] = [stmt]
    while todo:
        node = todo.pop()
        if isinstance(node, _SCOPES):
            if not isinstance(node, ast.Lambda):
                names.add(node.name)
            continue
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            names |= _bound_names(node)
            continue
        if isinstance(node, ast.Name) and isinstance(node.ctx, (ast.Store, ast.Del)):
            names.add(node.id)
        elif isinstance(node, (ast.Attribute, ast.Subscript)) and isinstance(node.ctx, (ast.Store, ast.Del)):
            if root := _chain_root(node):
                names.add(root)
        elif isinstance(node, ast.ExceptHandler) and node.name:
            names.add(node.name)
        elif (
            isinstance(node, ast.Expr)
            and isinstance(node.value, ast.Call)
            and isinstance(node.value.func, ast.Attribute)
            and (root := _chain_root(node.value.func.value))
        ):
            names.add(root)
        todo.extend(ast.iter_child_nodes(node))
    return names


def _module_bindings(source: str) -> dict[str, list[ast.stmt]]:
    """Name → every top-level statement binding or mutating it, in order."""
    bindings: dict[str, list[ast.stmt]] = {}
    for stmt in ast.parse(source).body:
        for name in _bound_names(stmt):
            bindings.setdefault(name, []).append(stmt)
    return bindings


def _reads(node: ast.AST) -> set[str]:
    """Names ``node`` resolves outside itself: loads and a def's parameters, minus its locals.

    Parameters count because a test's parameter is a fixture request, which
    a module-level fixture of that name in the lint home would answer.
    """
    loads = {n.id for n in ast.walk(node) if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Load)}
    if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return loads
    stored = {n.id for stmt in node.body for n in ast.walk(stmt) if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Store)}
    params = {a.arg for a in [*node.args.posonlyargs, *node.args.args, *node.args.kwonlyargs]}
    return (loads - stored) | params


def _path_constant(stmt: ast.stmt) -> bool:
    """An assignment of a path expression: `Path(…)`, `__file__`, `.parent(s)[…]`, `/`-joins, str parts."""
    if not isinstance(stmt, (ast.Assign, ast.AnnAssign)) or stmt.value is None:
        return False
    targets = stmt.targets if isinstance(stmt, ast.Assign) else [stmt.target]
    if not all(isinstance(t, ast.Name) for t in targets):
        return False
    marked = False
    for node in ast.walk(stmt.value):
        if isinstance(node, ast.Name):
            marked |= node.id == "__file__"
        elif isinstance(node, ast.Attribute):
            if node.attr in ("parent", "parents"):
                marked = True
            elif node.attr not in _PATH_METHODS:
                return False
        elif isinstance(node, ast.Call):
            func = node.func
            if node.keywords or not (
                (isinstance(func, ast.Name) and func.id in _PATH_FACTORIES)
                or (isinstance(func, ast.Attribute) and func.attr in _PATH_METHODS)
            ):
                return False
            marked |= isinstance(func, ast.Name)
        elif isinstance(node, ast.BinOp):
            if not isinstance(node.op, ast.Div):
                return False
            marked = True
        elif isinstance(node, ast.Constant):
            if type(node.value) not in (str, int):
                return False
        elif not isinstance(node, (ast.Subscript, ast.expr_context, ast.Div)):
            return False
    return marked


def _moved_global_problems(
    base_src: str, lint_src: str, qualname: str, lint_path: str, allow: frozenset[tuple[str, int]] = frozenset()
) -> list[str]:
    """Names the moved def ``qualname`` reads that the lint home binds differently from its source.

    A difference is admitted as a path constant on both sides, or as a
    reviewed rewrite: every lint-home statement binding the name allow-listed
    at its first line. Past either, the lint home's statements are what runs,
    so their reads are checked in turn.
    """
    base_defs = _qualified_defs(base_src)
    start: set[str] = {"*"} | _reads(base_defs[qualname])
    parts = qualname.split(".")
    for depth in range(1, len(parts)):
        cls = base_defs[".".join(parts[:depth])]
        if isinstance(cls, ast.ClassDef):
            own = [item for item in cls.body if not isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))]
            for node in [*cls.decorator_list, *cls.bases, *cls.keywords, *own]:
                start |= _reads(node)
    base_bind, lint_bind = _module_bindings(base_src), _module_bindings(lint_src)
    problems: list[str] = []
    # (name, reached only through a path constant's value). Past a path
    # constant only the lint home's spelling runs, and it may be built from
    # names the source never bound (`HERE = Path(__file__).parent`) — as long
    # as each of those is itself a path constant or identical.
    todo: list[tuple[str, bool]] = [(name, False) for name in sorted(start)]
    seen: dict[str, bool] = {}
    while todo:
        name, via_path = todo.pop()
        if name in seen and (not seen[name] or via_path):
            continue
        seen[name] = via_path
        was, now = base_bind.get(name, []), lint_bind.get(name, [])
        if [ast.dump(s) for s in was] == [ast.dump(s) for s in now]:
            todo.extend((read, via_path) for stmt in now for read in sorted(_reads(stmt)))
        elif now and all(map(_path_constant, now)) and (via_path or (was and all(map(_path_constant, was)))):
            todo.extend((read, True) for stmt in now for read in sorted(_reads(stmt)))
        elif now and all((lint_path, stmt.lineno) in allow for stmt in now):
            todo.extend((read, via_path) for stmt in now for read in sorted(_reads(stmt)))
        else:
            where = f"line {now[0].lineno}" if now else "unbound"
            problems.append(
                f"{lint_path}: moved `{qualname}` reads `{name}`, bound differently than in its source module "
                f"({where}) — a moved test runs against the same module-level names, only a path constant may change"
            )
    return problems


# Comments, strings and char literals in Rust source — the spans a brace
# counter must not read. Raw strings and nested block comments are closed by
# hand in `_mask_rust`, since neither is a regular language.
_RUST_SPAN = re.compile(
    r"(?P<line>//[^\n]*)"
    r"|(?P<block>/\*)"
    r"|(?<!\w)(?P<raw>b?r(?P<hashes>#*)\")"
    r"|(?P<str>b?\"(?:\\.|[^\"\\])*\")"
    r"|(?P<char>b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)|[^'\\\n])')",
    re.DOTALL,
)
_RUST_ITEM = re.compile(r"\b(?P<kind>fn|mod)\s+(?P<name>[A-Za-z_]\w*)")
# What may stand between a leading marker and its `fn`, once comments are
# masked and attributes removed: visibility and qualifiers.
_RUST_QUALIFIERS = re.compile(r"(?:\s|pub(?:\s*\([\w\s:]*\))?|async|unsafe|const|extern)*")


def _mask_rust(source: str) -> str:
    """`source` with every comment, string and char literal blanked, newlines kept."""
    out = list(source)

    def blank(start: int, end: int) -> None:
        for i in range(start, end):
            if out[i] != "\n":
                out[i] = " "

    pos = 0
    while m := _RUST_SPAN.search(source, pos):
        if m["block"]:
            depth, end = 1, m.end()
            while depth and end < len(source):
                if source.startswith("/*", end):
                    depth, end = depth + 1, end + 2
                elif source.startswith("*/", end):
                    depth, end = depth - 1, end + 2
                else:
                    end += 1
        elif m["raw"]:
            close = source.find('"' + m["hashes"], m.end())
            end = len(source) if close < 0 else close + 1 + len(m["hashes"])
        else:
            end = m.end()
        blank(m.start(), end)
        pos = end
    return "".join(out)


def _close_brace(masked: str, open_at: int) -> int:
    depth = 0
    for i in range(open_at, len(masked)):
        if masked[i] == "{":
            depth += 1
        elif masked[i] == "}":
            depth -= 1
            if not depth:
                return i
    return len(masked)


def _strip_attributes(text: str) -> str:
    """`text` without its `#[...]` / `#![...]` attributes (brackets balanced)."""
    out, i = [], 0
    while i < len(text):
        m = re.compile(r"#!?\[").match(text, i)
        if not m:
            out.append(text[i])
            i += 1
            continue
        depth, i = 1, m.end()
        while depth and i < len(text):
            depth += {"[": 1, "]": -1}.get(text[i], 0)
            i += 1
    return "".join(out)


def _rust_test_at(source: str, line: int) -> str | None:
    """The in-file libtest path (`tests::fn`) of the fn a marker on `line` belongs to.

    Its fn is the innermost one whose body holds the line, else the first fn
    after it when only attributes, comments and qualifiers stand between (the
    marker leads the test). The path prefixes every inline `mod x { … }`
    enclosing that fn. None when the marker belongs to no fn.
    """
    masked = _mask_rust(source)
    starts = [0]
    for text_line in source.splitlines(keepends=True):
        starts.append(starts[-1] + len(text_line))
    if line > len(starts) - 1:
        return None
    at, after = starts[line - 1], starts[line]
    items: list[tuple[str, str, int, int, int]] = []  # kind, name, keyword, open, close
    for m in _RUST_ITEM.finditer(masked):
        body = re.compile(r"[{;]").search(masked, m.end())
        if body and body.group() == "{":
            items.append((m["kind"], m["name"], m.start(), body.start(), _close_brace(masked, body.start())))
    fns = [item for item in items if item[0] == "fn"]
    inside = [item for item in fns if item[3] < at < item[4]]
    if inside:
        target = max(inside, key=lambda item: item[3])
    else:
        leading = [item for item in fns if item[2] >= after]
        if not leading:
            return None
        target = min(leading, key=lambda item: item[2])
        if not _RUST_QUALIFIERS.fullmatch(_strip_attributes(masked[after:target[2]])):
            return None
    mods = sorted((item for item in items if item[0] == "mod" and item[3] < target[2] < item[4]), key=lambda i: i[3])
    return "::".join([*(mod[1] for mod in mods), target[1]])


@dataclass(slots=True)
class Port:
    """One `ported-from` marker: where it is, and the libtest case it names."""

    where: str  # `<file>:<line>`
    crate: str | None = None  # `crates/<crate>/`
    stem: str | None = None  # `tests/<stem>.rs` target, None for `src/`
    name: str | None = None  # the full libtest path; None when underivable


def _libtest_name(rs_path: str, in_file: str) -> tuple[str, str | None, str] | None:
    """(crate dir, integration-test stem or None, libtest path), or None if underivable.

    `crates/<c>/src/lib.rs`/`main.rs` are the module root, `src/a/mod.rs` is
    `a`, `src/a/b.rs` is `a::b`; `crates/<c>/tests/<stem>.rs` is the root of
    its own target. Anything else (`src/bin/`, `benches/`, `#[path]`) is not
    derived, and an underivable port is refused rather than guessed.
    """
    parts = rs_path.split("/")
    if len(parts) < 4 or parts[0] != "crates":
        return None
    crate, where, rel = parts[1], parts[2], parts[3:]
    if where == "tests" and len(rel) == 1:
        return crate, rel[0].removesuffix(".rs"), in_file
    if where != "src" or rel[0] == "bin":
        return None
    if rel in (["lib.rs"], ["main.rs"]):
        module: list[str] = []
    elif rel[-1] == "mod.rs":
        module = rel[:-1]
    else:
        module = [*rel[:-1], rel[-1].removesuffix(".rs")]
    return crate, None, "::".join([*module, in_file])


def _ported_markers(repo: Path, base: str, head: str) -> dict[tuple[str, str], list[Port]]:
    """(module, qualname) → the ports of every `ported-from` marker added in the range."""
    diff = git(repo, "diff", "--no-color", "--no-renames", "--unified=0", base, head, "--", "*.rs")
    per_file: dict[str, list[str]] = {}
    current: list[str] = []
    in_header = False
    for line in diff.splitlines():
        if line.startswith("diff --git "):
            current, in_header = [], True
        elif in_header and line.startswith("+++ b/"):
            per_file[line[len("+++ b/"):]] = current = []
            in_header = False
        elif not in_header:
            current.append(line)
    found: dict[tuple[str, str], list[Port]] = {}
    for rs_path, lines in per_file.items():
        source: str | None = None
        for hunk in parse_hunks("\n".join(lines)):
            for lineno, text in hunk.added:
                if not (m := PORTED_FROM.match(text)):
                    continue
                if source is None:
                    source = git(repo, "show", f"{head}:{rs_path}")
                port = Port(where=f"{rs_path}:{lineno}")
                in_file = _rust_test_at(source, lineno)
                derived = _libtest_name(rs_path, in_file) if in_file else None
                if derived:
                    port.crate, port.stem, port.name = derived
                found.setdefault((m["module"], m["case"].replace("::", ".")), []).append(port)
    return found


def _port_verdict(port: Port, cases: list[ET.Element]) -> str | None:
    """Why this port's Rust test does not count as executed and passed, or None."""
    if port.name is None:
        return (
            "the marker belongs to no fn whose libtest path can be derived — put it in a test's body or "
            "leading attribute block, in crates/<crate>/src/** or crates/<crate>/tests/<stem>.rs"
        )
    label = f"//crates/{port.crate}:"

    def owned(classname: str) -> bool:
        if port.stem is not None:
            return classname == f"{label}{port.stem}"
        return classname.startswith(label) and classname.endswith("_test")

    hits = [c for c in cases if c.get("name") == port.name and owned(c.get("classname", ""))]
    test = f"`{port.name}` under {label}{port.stem or '*_test'}"
    if not hits:
        return (
            f"{test} is not in {UNIT_JUNIT}, so it never executed — put the marker in a test that runs on "
            "Linux under `//crates/...` (no cfg gate, no #[path]), named as libtest names it"
        )
    if any(c.find("skipped") is not None for c in hits):
        return f"{test} was skipped, and an #[ignore]d port executes nothing — remove the #[ignore]"
    if any(c.find("failure") is not None or c.find("error") is not None for c in hits):
        return f"{test} failed in the unit run — fix the port until it passes, then delete the original"
    return None


def _report_problems(
    repo: Path, head: str, ports: dict[tuple[str, str], list[Port]], tiered: Tiered
) -> tuple[set[tuple[str, str]], list[str]]:
    """(e) The ports whose Rust test executed and passed in a fresh UNIT_JUNIT at HEAD (AM-7).

    Fresh means for a tree containing the tip: the tip is an ancestor of HEAD
    (after P-5's `--no-ff` merge HEAD is the merge commit, never the tip), the
    working tree is HEAD exactly (`git status --porcelain` empty — a staged or
    unstaged edit is a tree HEAD does not hold, an untracked file one it does
    not carry), and the report was written by the unit run this guard started.
    """

    def refuse_all(why: str) -> tuple[set[tuple[str, str]], list[str]]:
        return set(), [
            f"{port.where}: ported-from {module}::{qualname.replace('.', '::')} "
            f"(Rust test `{port.name or '<underivable>'}`): {why}"
            for (module, qualname), found in ports.items()
            for port in found
        ]

    ancestor = subprocess.run(
        ["git", "-C", str(repo), "merge-base", "--is-ancestor", head, "HEAD"], capture_output=True, check=False
    ).returncode
    if ancestor != 0:
        return refuse_all(
            f"the range tip {head[:12]} is not an ancestor of HEAD, so a unit run here tests a tree "
            "without the port — check out the tip or its merge first"
        )
    dirty = git(repo, "status", "--porcelain", "--untracked-files=normal").splitlines()
    if dirty:
        return refuse_all(
            f"the working tree differs from HEAD ({', '.join(d.strip() for d in dirty[:3])}"
            f"{', …' if len(dirty) > 3 else ''}), so a unit run here would not test HEAD — commit or clean it"
        )
    # Freshness by construction rather than by mtime: an older report is
    # deleted first, so any report present afterwards is this run's. (An mtime
    # floor had a hole as wide as its rounding — a report written in the same
    # second as the run started read as fresh.)
    report = repo / UNIT_JUNIT
    report.unlink(missing_ok=True)
    status = tiered.unit_runner(repo)
    if status != 0:
        return refuse_all(
            f"`task bazel:test:unit` exited {status} at HEAD, and a failed unit run proves no port — "
            "make the unit lane green, then re-run the guard"
        )
    if not report.is_file():
        return refuse_all(
            f"stale or missing report: the unit run wrote no fresh {UNIT_JUNIT} (the previous one was deleted "
            "before it started) — run the guard where `task bazel:test:unit` writes its per-case report (Linux)"
        )
    try:
        cases = ET.parse(report).getroot().findall(".//testcase")
    except ET.ParseError as err:
        return refuse_all(f"{UNIT_JUNIT} is not readable JUnit ({err}) — re-run `task bazel:test:unit`")
    admitted: set[tuple[str, str]] = set()
    problems: list[str] = []
    for key, found in ports.items():
        # Every marker naming a case must hold: a marker is a claim of coverage.
        verdicts = [(port, _port_verdict(port, cases)) for port in found]
        failing = [(port, why) for port, why in verdicts if why]
        if not failing:
            admitted.add(key)
        for port, why in failing:
            problems.append(
                f"{port.where}: ported-from {key[0]}::{key[1].replace('.', '::')} "
                f"(Rust test `{port.name or '<underivable>'}`): {why}"
            )
    return admitted, problems


# Names pytest reads by name, never through a load the orphan test can see.
_NEVER_ORPHANED = re.compile(r"pytestmark|pytest_\w*|collect_ignore\w*|__\w+__")


def _orphan_names(stmt: ast.stmt, suite: frozenset[str]) -> list[str] | None:
    """The names `stmt` binds, when it is a removable kind; None when it is not.

    Removable: an import (not `__future__`), a plain assignment the DEC-10(b)
    walk finds nothing in, and an undecorated helper def. Never a test, a
    fixture, a hook, a class or a bare expression — those act by being there.
    """
    if isinstance(stmt, ast.Import):
        return [alias.asname or alias.name.split(".")[0] for alias in stmt.names]
    if isinstance(stmt, ast.ImportFrom):
        return None if stmt.module == "__future__" else [alias.asname or alias.name for alias in stmt.names]
    if isinstance(stmt, (ast.Assign, ast.AnnAssign)):
        targets = stmt.targets if isinstance(stmt, ast.Assign) else [stmt.target]
        if stmt.value is None or not all(isinstance(t, ast.Name) for t in targets):
            return None
        if _store_problems([stmt], {}, enter_defs=False, suite=suite) or any(
            _rebinding(node, {}) for node in ast.walk(stmt)
        ):
            return None
        return [t.id for t in targets if isinstance(t, ast.Name)]
    if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return None if stmt.decorator_list or stmt.name.startswith("test") else [stmt.name]
    return None


def _sanctioned_base_lines(base_src: str, head_src: str, sanctioned: frozenset[str], removed: set[int]) -> set[int]:
    """Base lines a sanctioned removal may take: the defs, emptied classes, orphaned statements.

    An orphan is a whole module-level statement the diff removes, of a kind
    `_orphan_names` admits, every one of whose names a sanctioned removed def
    — or another orphan, transitively — actually reads, none of which occurs as
    a word anywhere in the head module (code, strings and comments alike — a
    fixture is requested by parameter name and `usefixtures` by string, neither
    a load) and none of which pytest reads by name. "Read by what left" is the
    positive half: an import nothing reads (`from _support import leak_check
    # noqa: F401`, an autouse fixture registered by being imported) acts by
    being there, and its disappearance is not a consequence of the move. It is
    what ruff's F401 would otherwise force a move to leave behind.
    """
    base_defs = _qualified_defs(base_src)
    kept = set(_qualified_defs(head_src))
    lines: set[int] = set()
    for qualname in sanctioned:
        lines.update(_span(base_defs[qualname]))
    for qualname, node in base_defs.items():
        if not isinstance(node, ast.ClassDef) or qualname in kept:
            continue
        body = node.body[1:] if ast.get_docstring(node) is not None else node.body
        if body and all(
            isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)) and f"{qualname}.{item.name}" in sanctioned
            for item in body
        ):
            lines.update(_span(node))
    def reads(node: ast.AST) -> set[str]:
        return {n.id for n in ast.walk(node) if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Load)}

    suite = _suite_modules(base_src)
    candidates: list[tuple[ast.stmt, set[int], list[str]]] = []
    for stmt in ast.parse(base_src).body:
        span = set(_span(stmt)) if hasattr(stmt, "decorator_list") else set(
            range(stmt.lineno, (stmt.end_lineno or stmt.lineno) + 1)
        )
        names = _orphan_names(stmt, suite) if span <= removed else None
        if names and not any(
            _NEVER_ORPHANED.fullmatch(name) or re.search(rf"(?<!\w){re.escape(name)}(?!\w)", head_src)
            for name in names
        ):
            candidates.append((stmt, span, names))
    read = set().union(*(reads(base_defs[q]) for q in sanctioned))
    grew = True
    while grew:
        grew = False
        for entry in list(candidates):
            stmt, span, names = entry
            if set(names) <= read:
                lines.update(span)
                read |= reads(stmt)
                candidates.remove(entry)
                grew = True
    return lines


def _is_command_mark(node: ast.expr) -> bool:
    """`pytest.mark.command("<key>", ...)` — string literals only, no keyword."""
    return (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "command"
        and isinstance(node.func.value, ast.Attribute)
        and node.func.value.attr == "mark"
        and isinstance(node.func.value.value, ast.Name)
        and node.func.value.value.id == "pytest"
        and bool(node.args)
        and all(isinstance(a, ast.Constant) and isinstance(a.value, str) for a in node.args)
        and not node.keywords
    )


def _is_pytestmark(node: ast.expr, ctx: type) -> bool:
    return isinstance(node, ast.Name) and node.id == "pytestmark" and isinstance(node.ctx, ctx)


def _binds_pytestmark(stmt: ast.stmt) -> bool:
    """A top-level statement that binds or extends `pytestmark`."""
    if isinstance(stmt, (ast.Assign, ast.AugAssign, ast.AnnAssign)):
        targets = stmt.targets if isinstance(stmt, ast.Assign) else [stmt.target]
        return any(_is_pytestmark(t, ast.Store) for t in targets)
    return (
        isinstance(stmt, ast.Expr)
        and isinstance(stmt.value, ast.Call)
        and isinstance(stmt.value.func, ast.Attribute)
        and _is_pytestmark(stmt.value.func.value, ast.Load)
    )


def _mark_state(tree: ast.Module) -> str | None:
    """What the module binds: "none", "single", "list" — None when it binds it out of reach.

    A `pytestmark` stored anywhere but a top-level statement (inside an `if`,
    a `try`, a def) cannot be keyed, so no form is admitted over it.
    """
    top = [stmt for stmt in tree.body if _binds_pytestmark(stmt)]
    stores = sum(1 for node in ast.walk(tree) if _is_pytestmark(node, ast.Store))
    if stores != sum(1 for stmt in top if not isinstance(stmt, ast.Expr)):
        return None
    if not top:
        return "none"
    last = top[-1]
    if isinstance(last, ast.Assign) and not isinstance(last.value, (ast.List, ast.Tuple)):
        return "single"
    return "list"


def _marker_form_ok(stmt: ast.stmt, state: str) -> bool:
    """The one form C-007(c) admits for `state`."""
    if state == "list":
        call = stmt.value if isinstance(stmt, ast.Expr) else None
        return (
            isinstance(call, ast.Call)
            and isinstance(call.func, ast.Attribute)
            and call.func.attr == "append"
            and _is_pytestmark(call.func.value, ast.Load)
            and len(call.args) == 1
            and not call.keywords
            and _is_command_mark(call.args[0])
        )
    if not (isinstance(stmt, ast.Assign) and len(stmt.targets) == 1 and _is_pytestmark(stmt.targets[0], ast.Store)):
        return False
    if state == "none":
        return _is_command_mark(stmt.value)
    return (
        isinstance(stmt.value, ast.List)
        and len(stmt.value.elts) == 2
        and _is_pytestmark(stmt.value.elts[0], ast.Load)
        and _is_command_mark(stmt.value.elts[1])
    )


def _marker_statement(base_src: str, head_src: str, hunks: list[Hunk]) -> tuple[set[int], list[str]]:
    """(c) Head lines of the one admitted `pytest.mark.command` statement, and the problems.

    A candidate is a wholly added top-level statement that binds `pytestmark`
    or names `pytest.mark.command`; anything else added is left to the line
    checks. The form is keyed by what the BASE module binds (`_mark_state`),
    so the existing `skipif` is carried forward by the bare name and never
    edited — a rewrite of it is a removed line in the marker's hunk.
    """
    added = {n for h in hunks for n, _ in h.added}
    head_tree = ast.parse(head_src)

    def names_command(stmt: ast.stmt) -> bool:
        return any(
            isinstance(node, ast.Attribute) and node.attr == "command"
            and isinstance(node.value, ast.Attribute) and node.value.attr == "mark"
            for node in ast.walk(stmt)
        )

    candidates = [
        stmt
        for stmt in head_tree.body
        # A new def carrying `@pytest.mark.command` is a new test, judged as one.
        if not isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
        and set(range(stmt.lineno, (stmt.end_lineno or stmt.lineno) + 1)) <= added
        and (_binds_pytestmark(stmt) or names_command(stmt))
    ]
    if not candidates:
        return set(), []
    if len(candidates) > 1:
        return set(), [
            f"line {stmt.lineno}: more than one added pytestmark statement — C-007(c) admits one per module"
            for stmt in candidates
        ]
    stmt = candidates[0]
    span = set(range(stmt.lineno, (stmt.end_lineno or stmt.lineno) + 1))
    text = ast.unparse(stmt)
    # The marker's hunk is the one whose NEW side holds its lines. `covers`
    # also matches the old side, and `span` is head numbering: over a range
    # where an earlier hunk removed lines, an unrelated removal hunk's old
    # line numbers overlap the marker's new ones (WP-06; branch-wide over
    # test_doc_scripts_publish.py, `-80,2 +66,0` against a marker at 77-104).
    if any(h.removed and h.new_start <= n < h.new_start + h.new_len for h in hunks for n in span):
        return set(), [f"line {stmt.lineno}: the marker's hunk also removes a line — the existing binding is never edited: {text!r}"]
    state = _mark_state(ast.parse(base_src))
    if state is None:
        return set(), [f"line {stmt.lineno}: the module binds pytestmark below top level, so no marker form applies: {text!r}"]
    if not _marker_form_ok(stmt, state):
        expected = {
            "none": 'pytestmark = pytest.mark.command("<key>", ...)',
            "single": 'pytestmark = [pytestmark, pytest.mark.command("<key>", ...)]',
            "list": 'pytestmark.append(pytest.mark.command("<key>", ...))',
        }[state]
        return set(), [
            (
                f"line {stmt.lineno}: not the admitted marker line for a module binding {state} "
                f"pytestmark — only `{expected}` with string-literal arguments: {text!r}"
            )
        ]
    later = [s.lineno for s in head_tree.body if s is not stmt and _binds_pytestmark(s) and s.lineno > stmt.lineno]
    if later:
        return set(), [f"line {stmt.lineno}: the marker must follow the module's last pytestmark binding (line {later[-1]}): {text!r}"]
    return span, []


def _nodeid(path: str, qualname: str) -> str:
    """pytest's node id relative to `test/` (the rootdir): `tests/x.py::Class::test`."""
    return f"{path.removeprefix('test/')}::{qualname.replace('.', '::')}"


# (b) A lint conftest governs collection of the moved tests, so the ways it
# could make them never run are refused by name: every collection and
# run-protocol hook below, `collect_ignore*`, any skip/xfail, and a plugin
# other than `pytester` (C-008's own). Admission hooks — `pytest_runtest_setup`
# raising, `pytest_sessionfinish` failing the session, fixtures — stay open.
_LINT_CONFTEST_HOOKS = frozenset(
    {
        "pytest_collection",
        "pytest_collection_modifyitems",
        "pytest_ignore_collect",
        "pytest_collect_file",
        "pytest_collect_directory",
        "pytest_pycollect_makemodule",
        "pytest_pycollect_makeitem",
        "pytest_generate_tests",
        "pytest_deselected",
        "pytest_pyfunc_call",
        "pytest_runtest_call",
        "pytest_runtest_protocol",
        "pytest_runtest_makereport",
        "pytest_runtest_logreport",
        "pytest_report_teststatus",
    }
)
# Beyond the named hooks, the ways a conftest flips a verdict rather than a
# collection: pytest's private and unittest's skip machinery, a hook wrapper
# (which sees and can replace any hook's result), a store to a report's or a
# session's outcome, and ending the process early with a chosen status.
_LINT_PRIVATE_ROOTS = frozenset({"_pytest", "unittest"})
_LINT_SKIP_NAMES = frozenset({"SkipTest", "Skipped", "skip", "skipif", "xfail", "importorskip"})
_LINT_VERDICT_ATTRS = frozenset({"outcome", "passed", "failed", "skipped", "exitstatus"})
_LINT_EXIT_NAMES = frozenset({"exit", "_exit", "abort", "kill", "quit"})
_LINT_PLUGINS = frozenset({"pytester"})
_SKIPPING = frozenset({"skip", "skipif", "xfail", "importorskip"})


def _skip_hits(nodes: list[ast.stmt], imports: dict[str, str]) -> list[str]:
    """`line N: …` for every skip/xfail reachable from `nodes`.

    `pytest.mark.skip(if)`/`xfail` and `pytest.skip`/`xfail`/`importorskip`
    by attribute from the `pytest` root, and the same names imported bare from
    pytest (`from pytest import skip`).
    """
    hits = []
    for stmt in nodes:
        for node in ast.walk(stmt):
            if isinstance(node, ast.Attribute) and node.attr in _SKIPPING and _root_name(node) == "pytest":
                hits.append(f"line {node.lineno}: `{ast.unparse(node)}` — a lint tier that skips runs nothing")
            elif isinstance(node, ast.Name) and node.id in _SKIPPING and imports.get(node.id) == "pytest":
                hits.append(f"line {node.lineno}: `{node.id}` from pytest — a lint tier that skips runs nothing")
    return hits


def _failing_status(value: ast.expr) -> bool:
    """A status that can only fail a session: a nonzero int, or `pytest.ExitCode.<not OK>`."""
    if isinstance(value, ast.Constant):
        return type(value.value) is int and value.value != 0
    return (
        isinstance(value, ast.Attribute)
        and value.attr != "OK"
        and isinstance(value.value, ast.Attribute)
        and value.value.attr == "ExitCode"
        and _root_name(value) == "pytest"
    )


def _is_wrapper_hookimpl(decorator: ast.expr) -> bool:
    """`@pytest.hookimpl(wrapper=…)` / `(hookwrapper=…)` with anything but a literal False."""
    if not isinstance(decorator, ast.Call):
        return False
    func = decorator.func
    if (func.attr if isinstance(func, ast.Attribute) else getattr(func, "id", None)) != "hookimpl":
        return False
    return any(
        kw.arg in ("wrapper", "hookwrapper") and not (isinstance(kw.value, ast.Constant) and kw.value.value is False)
        for kw in decorator.keywords
    )


# The execution state an admitted hook must not write: pytest hands every hook
# its node, session and config objects, and `item.obj = lambda: None` in a
# `pytest_runtest_setup` turns every failing test into a passing one while
# collection counts and skip ceilings stay exact. So a name holding one — a
# parameter under a pytest hook/fixture argument name, or anything derived
# from one by attribute, subscript, method result or unpacking — is read-only:
# no store or `del` rooted at it, no method on it outside `_HOOK_READ_METHODS`,
# never handed to a call that is not a pure builtin, and `getattr` on it only
# for `_HOOK_READ_ATTRS`. `pytest` itself is a root for the store and argument
# rules (`pytest.Function.runtest = …`, `monkeypatch.setattr(pytest.Function, …)`).
_HOOK_OBJECT_PARAMS = frozenset(
    {
        "item", "items", "session", "config", "early_config", "request", "pyfuncitem", "collector",
        "report", "reports", "call", "node", "metafunc", "parser", "pluginmanager", "fixturedef",
        "excinfo", "nextitem", "parent", "manager", "plugin", "terminalreporter", "module", "fixturemanager",
    }
)
_HOOK_READ_METHODS = frozenset(
    {"get", "get_plugin", "getoption", "getini", "getvalue", "get_closest_marker", "iter_markers",
     "write_sep", "write_line", "keys", "values", "startswith", "endswith", "split", "strip", "format"}
)
_HOOK_READ_ATTRS = frozenset({"fixturenames", "nodeid", "name", "originalname", "path", "location"})
# Builtins a hook object may be handed to: they read it, and a container
# they return still carries it (`_carries`), so the taint follows.
_PURE_CALLS = frozenset(
    {"getattr", "hasattr", "isinstance", "len", "str", "repr", "bool", "id", "sorted", "set",
     "frozenset", "list", "tuple", "print", "float", "int"}
)
# Hooks registered under another name, which the named-hook refusal above
# would never see: a plugin object handed to the plugin manager.
_HOOK_REGISTRATION_ATTRS = frozenset(
    {"register", "unregister", "import_plugin", "consider_module", "consider_pluginarg", "consider_conftest",
     "consider_env", "consider_preparse", "add_hookspecs", "add_hookcall_monitoring", "subset_hook_caller",
     "load_setuptools_entrypoints"}
)


# Calls whose result is a fresh scalar, never the object passed in.
_SCALAR_CALLS = frozenset({"str", "repr", "len", "bool", "int", "float", "isinstance", "hasattr", "id"})


def _carries(node: ast.expr | None, roots: set[str]) -> bool:
    """Whether evaluating ``node`` can yield an object reachable from a name in ``roots``.

    Conservative: any such name inside counts, through containers, calls,
    lambdas and operators alike, except under what can only produce a fresh
    scalar — an f-string, a comparison, `not`, a scalar builtin — or a read of
    a `_HOOK_READ_ATTRS` attribute (`item.nodeid`, `getattr(item, "fixturenames")`).
    """
    if node is None:
        return False
    todo: list[ast.AST] = [node]
    while todo:
        cur = todo.pop()
        if isinstance(cur, (ast.JoinedStr, ast.Compare)) or (
            isinstance(cur, ast.UnaryOp) and isinstance(cur.op, ast.Not)
        ):
            continue
        if isinstance(cur, ast.Attribute) and isinstance(cur.ctx, ast.Load) and cur.attr in _HOOK_READ_ATTRS:
            continue
        if isinstance(cur, ast.Call) and isinstance(cur.func, ast.Name):
            if cur.func.id in _SCALAR_CALLS:
                continue
            if cur.func.id == "getattr" and len(cur.args) > 1 and isinstance(cur.args[1], ast.Constant) and (
                cur.args[1].value in _HOOK_READ_ATTRS
            ):
                continue
        if isinstance(cur, ast.Name) and cur.id in roots:
            return True
        todo.extend(ast.iter_child_nodes(cur))
    return False


def _hook_state_problems(tree: ast.Module, admitted_stores: set[int]) -> list[str]:
    """Writes to pytest's execution state through a hook object (see `_HOOK_OBJECT_PARAMS`)."""
    tainted = {a.arg for a in ast.walk(tree) if isinstance(a, ast.arg) and a.arg in _HOOK_OBJECT_PARAMS}
    grew = True
    while grew:
        before = len(tainted)
        for node in ast.walk(tree):
            pairs: list[tuple[ast.expr, ast.expr | None]] = []
            if isinstance(node, ast.Assign):
                pairs = [(t, node.value) for t in node.targets]
            elif isinstance(node, (ast.AnnAssign, ast.AugAssign, ast.NamedExpr)):
                pairs = [(node.target, node.value)]
            elif isinstance(node, (ast.For, ast.AsyncFor, ast.comprehension)):
                pairs = [(node.target, node.iter)]
            elif isinstance(node, ast.withitem) and node.optional_vars is not None:
                pairs = [(node.optional_vars, node.context_expr)]
            for target, value in pairs:
                if _carries(value, tainted):
                    tainted |= {n.id for n in ast.walk(target) if isinstance(n, ast.Name)}
        grew = len(tainted) > before
    store_roots = tainted | {"pytest"}
    stash_keys = {
        t.id
        for stmt in tree.body
        if isinstance(stmt, ast.Assign) and isinstance(stmt.value, ast.Call)
        and ast.unparse(stmt.value.func).startswith("pytest.StashKey")
        for t in stmt.targets if isinstance(t, ast.Name)
    }
    problems: list[str] = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.Global, ast.Nonlocal)):
            problems.append(f"line {node.lineno}: `{ast.unparse(node)}` — a lint conftest keeps no hook object past its call")
        if isinstance(node, (ast.Attribute, ast.Subscript)) and isinstance(node.ctx, (ast.Store, ast.Del)):
            own_stash = (
                isinstance(node, ast.Subscript) and isinstance(node.ctx, ast.Store)
                and isinstance(node.value, ast.Attribute) and node.value.attr == "stash"
                and isinstance(node.slice, ast.Name) and node.slice.id in stash_keys
            )
            if _carries(node.value, store_roots) and id(node) not in admitted_stores and not own_stash:
                problems.append(
                    f"line {node.lineno}: writes `{ast.unparse(node)}` — a hook object is read-only to a lint "
                    "conftest (only `session.exitstatus = <failing>` and its own stash key are written)"
                )
        if isinstance(node, ast.Attribute) and node.attr in _HOOK_REGISTRATION_ATTRS:
            problems.append(f"line {node.lineno}: `{ast.unparse(node)}` registers hooks under another name")
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        on_hook_object = isinstance(func, ast.Attribute) and _carries(func.value, tainted)
        if on_hook_object and func.attr not in _HOOK_READ_METHODS:
            problems.append(f"line {node.lineno}: calls `{ast.unparse(func)[:60]}` on a hook object — only reads are admitted")
        if isinstance(func, ast.Name) and func.id == "getattr" and node.args and _carries(node.args[0], tainted) and not (
            len(node.args) > 1 and isinstance(node.args[1], ast.Constant) and node.args[1].value in _HOOK_READ_ATTRS
        ):
            problems.append(f"line {node.lineno}: `{ast.unparse(node)[:80]}` — getattr on a hook object reads only {sorted(_HOOK_READ_ATTRS)}")
        if not (on_hook_object or (isinstance(func, ast.Name) and func.id in _PURE_CALLS)):
            for arg in [*node.args, *(kw.value for kw in node.keywords)]:
                if _carries(arg, store_roots):
                    problems.append(
                        f"line {node.lineno}: hands `{ast.unparse(arg)[:60]}` to `{ast.unparse(func)[:40]}` — a hook "
                        "object may reach only a pure builtin"
                    )
        if isinstance(func, ast.Attribute) and func.attr in ("setattr", "delattr", "setitem", "delitem") and node.args and (
            isinstance(node.args[0], ast.Constant)
        ):
            problems.append(f"line {node.lineno}: `{ast.unparse(node)[:80]}` patches by dotted string — the target is unjudgeable")
    return problems


def _lint_conftest_problems(source: str) -> list[str]:
    """(b) What `test/lint/conftest.py` may not do to the moved tests' collection or verdicts.

    Admission is left open — a `pytest_runtest_setup` that raises, fixtures,
    an autouse `monkeypatch` wrapper — and so is the budget hook, as long as
    the status it stores can only fail the session (`_failing_status`).
    """
    tree = ast.parse(source)
    problems = _skip_hits(tree.body, _import_bindings(source))
    admitted_stores: set[int] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import) and any(a.name.split(".")[0] in _LINT_PRIVATE_ROOTS for a in node.names):
            problems.append(f"line {node.lineno}: imports {_LINT_PRIVATE_ROOTS & {a.name.split('.')[0] for a in node.names}} — skip machinery outside pytest's public API")
        elif isinstance(node, ast.ImportFrom) and (node.module or "").split(".")[0] in _LINT_PRIVATE_ROOTS:
            problems.append(f"line {node.lineno}: imports from `{node.module}` — skip machinery outside pytest's public API")
        if (isinstance(node, ast.Name) and node.id in _LINT_SKIP_NAMES) or (
            isinstance(node, ast.Attribute) and node.attr in _LINT_SKIP_NAMES and _root_name(node) != "pytest"
        ):
            problems.append(f"line {node.lineno}: `{ast.unparse(node)}` — a lint tier that skips runs nothing")
        if (
            isinstance(node, ast.Name)
            and node.id in (_LINT_EXIT_NAMES | {"setattr", "delattr", "vars", "globals", "__import__", "exec", "eval"})
        ) or (
            isinstance(node, ast.Attribute)
            and node.attr in (_LINT_EXIT_NAMES | {"__setattr__", "__delattr__", "__setitem__", "__delitem__", "__dict__"})
        ):
            problems.append(f"line {node.lineno}: `{ast.unparse(node)}` can end the run or reach an outcome by name")
        if isinstance(node, ast.Attribute) and isinstance(node.ctx, ast.Store) and node.attr in _LINT_VERDICT_ATTRS:
            store = next(
                (a for a in ast.walk(tree) if isinstance(a, (ast.Assign, ast.AugAssign, ast.AnnAssign))
                 and node in (a.targets if isinstance(a, ast.Assign) else [a.target])),
                None,
            )
            admitted = (
                node.attr == "exitstatus" and isinstance(store, ast.Assign) and _failing_status(store.value)
            )
            if admitted:
                admitted_stores.add(id(node))
            else:
                problems.append(
                    f"line {node.lineno}: stores `{ast.unparse(node)}` — a conftest may only fail a session "
                    "(a nonzero int or pytest.ExitCode.<not OK>), never set an outcome"
                )
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "setattr" and any(
            isinstance(a, ast.Constant) and a.value in _LINT_VERDICT_ATTRS for a in node.args
        ):
            problems.append(f"line {node.lineno}: `{ast.unparse(node)[:80]}` sets an outcome by name")
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and any(
            _is_wrapper_hookimpl(d) for d in node.decorator_list
        ):
            problems.append(f"line {node.lineno}: `{node.name}` is a hook wrapper — it sees and can replace any hook's result")
        if isinstance(node, ast.keyword) and node.arg == "specname":
            problems.append(f"line {node.value.lineno}: `specname=` implements a hook under another name")
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name in _LINT_CONFTEST_HOOKS:
            problems.append(f"line {node.lineno}: hook `{node.name}` can drop or neuter collected tests")
        elif isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store) and node.id.startswith("collect_ignore"):
            problems.append(f"line {node.lineno}: `{node.id}` drops files from collection")
        elif isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store) and node.id == "pytest_plugins":
            value = next((a.value for a in ast.walk(tree) if isinstance(a, ast.Assign) and node in a.targets), None)
            names = value.elts if isinstance(value, (ast.List, ast.Tuple)) else [value]
            if not all(isinstance(n, ast.Constant) and n.value in _LINT_PLUGINS for n in names):
                problems.append(f"line {node.lineno}: `pytest_plugins` may name only {sorted(_LINT_PLUGINS)}")
    return problems + _hook_state_problems(tree, admitted_stores)


MoveSources = dict[str, tuple[set[tuple[str, str]], set[str]]]


def _carriable(stmt: ast.stmt) -> bool:
    """A module-level statement a lint module may carry verbatim from a source module.

    Every statement but a class, a test def and a decorated def: those are the
    moved tests themselves (matched by `_move_key`) or fixtures and hooks,
    which act by being there. An undecorated helper def is code the suite
    already ran — carried only when its `ast.dump` is identical, so a helper
    whose body changed on the way is walked as new code.
    """
    if isinstance(stmt, ast.ClassDef):
        return False
    if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return not stmt.decorator_list and not stmt.name.startswith("test")
    return True


def _move_sources(repo: Path, base: str, head: str, listing: list[tuple[str, str]]) -> MoveSources:
    """What a lint module may carry without judgement: code already in the suite.

    The `_move_key` of every test def removed from `test/tests/` in the range,
    and the `ast.dump` of every `_carriable` statement of a module those
    removals came from (a verbatim `skipif`, a `sys.path` line, a helper) — so the
    DEC-10(b) walk and the skip scan judge only what the move adds.
    """
    sources: MoveSources = {}
    for status, path in listing:
        if status not in ("D", "M") or not path.startswith("test/tests/") or not path.endswith(".py"):
            continue
        base_src = git(repo, "show", f"{base}:{path}")
        removed = _removed_test_defs(base_src, None if status == "D" else git(repo, "show", f"{head}:{path}"))
        if not removed:
            continue
        defs = _qualified_defs(base_src)
        sources[path] = (
            {_move_key(defs, qualname) for qualname in removed},
            {
                ast.dump(stmt)
                for stmt in ast.parse(base_src).body
                if _carriable(stmt)
            },
        )
    return sources


def _lint_module_problems(source: str, sources: MoveSources) -> list[str]:
    """(b) A new `test/lint/test_*.py`: DEC-10(b)'s whole-module walk and a skip
    scan over what it adds — every def that is not a moved twin and every
    `_carriable` statement (helper defs included) not copied verbatim from a source module whose defs
    THIS file carries (so one module's `skipif` cannot ride onto another's
    moved tests in the same range)."""
    keys = set().union(*(k for k, _ in sources.values()))
    tree = ast.parse(source)
    defs = _qualified_defs(source)
    carried: set[int] = set()
    for qualname, node in defs.items():
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and _move_key(defs, qualname) in keys:
            carried.update(_span(node))
            # The key matched each enclosing class's own context too, so that
            # context (decorators, header, non-def body) is carried as well.
            parts = qualname.split(".")
            for depth in range(1, len(parts)):
                cls = defs[".".join(parts[:depth])]
                carried.update(range(_span(cls).start, cls.body[0].lineno))
                for item in cls.body:
                    if not isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                        carried.update(range(item.lineno, (item.end_lineno or item.lineno) + 1))
    own_keys = {
        _move_key(defs, q) for q, n in defs.items() if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
    }
    statements = set().union(*(stmts for k, stmts in sources.values() if k & own_keys))
    for stmt in tree.body:
        if _carriable(stmt) and ast.dump(stmt) in statements:
            carried.update(range(stmt.lineno, (stmt.end_lineno or stmt.lineno) + 1))
    found = module_rebinding_problems(source) + _skip_hits(tree.body, _import_bindings(source))
    return [p for p in found if not ((m := re.match(r"line (\d+):", p)) and int(m[1]) in carried)]


def _tiered_removals(
    repo: Path,
    base: str,
    head: str,
    listing: list[tuple[str, str]],
    tiered: Tiered,
    allow: frozenset[tuple[str, int]] = frozenset(),
) -> tuple[dict[str, frozenset[str]], list[str], list[str]]:
    """(a) + (e): path → sanctioned qualnames, the problems, the sanctioned pytest node ids.

    A deleted module appears in the result only when it held at least one
    test def and every one of them is sanctioned — so its path being present
    is the whole-module sanction `check_range` reads.
    """
    lint = _added_lint_defs(repo, base, head, listing)
    removed: dict[str, dict[str, ast.AST]] = {}
    base_defs: dict[str, dict[str, ast.AST]] = {}
    base_srcs: dict[str, str] = {}
    deleted: set[str] = set()
    problems: list[str] = []
    for status, path in listing:
        if status not in ("D", "M") or not path.startswith("test/tests/") or not path.endswith(".py"):
            continue
        head_src = None if status == "D" else git(repo, "show", f"{head}:{path}")
        base_src = git(repo, "show", f"{base}:{path}")
        base_srcs[path] = base_src
        base_defs[path] = _qualified_defs(base_src)
        found = _removed_test_defs(base_src, head_src)
        if found or status == "D":
            removed[path] = found
        if status == "D":
            deleted.add(path)
    sanctioned: dict[str, set[str]] = {path: set() for path in removed}
    unmatched: list[tuple[str, str]] = []
    for path, defs in removed.items():
        for qualname in defs:
            key = _move_key(base_defs[path], qualname)
            if lint.get(key):
                lint_path = lint[key].pop()
                problems += _moved_global_problems(
                    base_srcs[path], git(repo, "show", f"{head}:{lint_path}"), qualname, lint_path, allow
                )
                sanctioned[path].add(qualname)
            else:
                unmatched.append((path, qualname))
    if unmatched:
        markers = _ported_markers(repo, base, head)
        wanted = {key: markers[key] for key in unmatched if key in markers}
        if wanted:
            admitted, port_problems = _report_problems(repo, head, wanted, tiered)
            problems += port_problems
            for path, qualname in admitted:
                sanctioned[path].add(qualname)
    result: dict[str, frozenset[str]] = {}
    for path, names in sanctioned.items():
        if path in deleted:
            missing = sorted(set(removed[path]) - names)
            if not removed[path] or missing:
                problems.append(
                    f"{path}: deleted, but "
                    + (f"{missing} have" if missing else "it holds no test def that")
                    + " no identical move to test/lint/ and no executed ported-from port"
                )
                continue
        if names:
            result[path] = frozenset(names)
    nodeids = [_nodeid(path, qualname) for path, names in result.items() for qualname in sorted(names)]
    return result, problems, nodeids


def _floor_pair(repo: Path, base: str, head: str, floor: str) -> tuple[int, int] | str | None:
    """`floor`'s (base, head) counts when the range modified it, else None; a problem string when either is no count."""
    if git(repo, "diff", "--name-status", "--no-renames", base, head, "--", floor).split()[:1] != ["M"]:
        return None
    values = []
    for rev in (base, head):
        text = git(repo, "show", f"{rev}:{floor}").strip()
        if not text.isdigit():
            return f"{floor}: {text!r} at {rev[:12]} is not a count"
        values.append(int(text))
    return values[0], values[1]


def _floor_problems(repo: Path, base: str, head: str, nodeids: list[str], tiered: Tiered) -> list[str]:
    """A `test/SUITE_FLOOR` decrease may not exceed the collected count of `nodeids`;
    a `test/LINT_FLOOR` decrease is refused outright.

    `≤`, where C-007 reads "equal": the merge-base range carries every
    in-series floor RAISE as well (a WP adding tests lifts the floor in the
    same series), so equality would red it forever once a raise and a move
    have both landed. A smaller decrease leaves a stricter floor, which the
    suite's own floor check reds if it is wrong — the direction that hides
    nothing. The count is taken once, at the base.

    The lint floor has no sanctioned decrease: nothing leaves `test/lint/`
    under any shape (a deletion there is refused, and an edit is judged as
    any collected module's), so a lower floor only hides lint tests that
    stopped collecting.
    """
    problems: list[str] = []
    lint = _floor_pair(repo, base, head, LINT_FLOOR)
    if isinstance(lint, str):
        problems.append(lint)
    elif lint and lint[1] < lint[0]:
        problems.append(
            f"{LINT_FLOOR}: lowered ({lint[0]} -> {lint[1]}) — no shape moves a test out of test/lint/, "
            "so the lint floor only rises"
        )
    floor = "test/SUITE_FLOOR"
    pair = _floor_pair(repo, base, head, floor)
    if isinstance(pair, str):
        return [*problems, pair]
    if pair is None or pair[0] <= pair[1]:
        return problems
    drop = pair[0] - pair[1]
    count = tiered.collector(repo, base, nodeids) if nodeids else 0
    if drop > count:
        problems.append(
            f"{floor}: lowered by {drop} ({pair[0]} -> {pair[1]}), but the sanctioned moves and ports "
            f"remove {count} collected case(s) — a floor may drop by no more than the tests the range "
            "moved to test/lint/ or ported away"
        )
    return problems


def check_range(
    repo: Path,
    base: str,
    head: str,
    allow: frozenset[tuple[str, int]],
    tiered: Tiered | None = None,
) -> list[str]:
    """Every violation in `base..head` under `test/`, as `path:line: reason` strings.

    `tiered` is `--tiered-shapes`: None is today's guard, byte for byte.
    """
    problems: list[str] = []
    listing = [
        (status, path)
        for status, _, path in (
            row.partition("\t")
            for row in git(repo, "diff", "--name-status", "--no-renames", base, head, "--", "test/").splitlines()
        )
    ]
    config = ALLOWED_CONFIG | TIERED_CONFIG if tiered else ALLOWED_CONFIG
    sanctioned: dict[str, frozenset[str]] = {}
    sources: MoveSources | None = None
    if tiered:
        sanctioned, removal_problems, nodeids = _tiered_removals(repo, base, head, listing, tiered, allow)
        problems.extend(removal_problems)
        problems.extend(_floor_problems(repo, base, head, nodeids, tiered))
    for status, path in listing:
        if status == "D":
            if not (tiered and path in sanctioned):
                problems.append(f"{path}: deleted — the suite never shrinks")
        elif path in config:
            problems.extend(check_config_edit(repo, base, head, path, tiered=bool(tiered)))
        elif status == "A" and tiered and path.startswith(LINT_DIR):
            if not _lint_file_allowed(path):
                problems.append(
                    f"{path}: only test/lint/conftest.py and test/lint/test_*.py may be added under "
                    "test/lint/ (no __init__.py, no data files, no subdirectories)"
                )
            elif path == f"{LINT_DIR}conftest.py":
                problems.extend(f"{path}:{p}" for p in _lint_conftest_problems(git(repo, "show", f"{head}:{path}")))
            else:
                if sources is None:
                    sources = _move_sources(repo, base, head, listing)
                problems.extend(
                    f"{path}:{p}" for p in _lint_module_problems(git(repo, "show", f"{head}:{path}"), sources)
                )
        elif status == "A":
            if _new_file_allowed(path):
                # The body rule is a Python AST rule about what a *test module*
                # may touch. A Bazel file is Starlark: `attr.bool()` and
                # `native.genrule()` are its ordinary vocabulary, and parsing
                # them under DEC-10(b) reports six violations on a correct
                # file. Their content has its own gate - `//:buildifier.check`
                # - so this rule judges only the Python it was written for.
                if path.endswith(".py"):
                    problems.extend(_module_problems(repo, head, path))
            else:
                problems.append(
                    f"{path}: only a new test/tests/**/*.py module may be added under test/ "
                    f"(never {sorted(_NEVER_NEW)}, nothing under test/src/, no data files)"
                )
        elif status == "M":
            if path == "test/conftest.py":
                # Stays frozen outright, and measurably so rather than by
                # tradition: it names zero crate paths, so the split never moves
                # a tree under it and the collision below cannot arise here.
                # Session bootstrap — `pytest_sessionstart`, the registry and
                # binary fixtures — where the cheapest edit is still behaviour.
                problems.append(f"{path}: test/conftest.py is session bootstrap — not editable")
            elif _fixture_only(path):
                # DEC-51's predicted collision, discharged. Frozen *semantics*,
                # not frozen bytes. The collected tests import these modules, so
                # every code line stays refused — but a docstring naming
                # `crates/ocx_lib/src/oci/index/wire.rs` is prose about a tree
                # this refactor moves, and freezing prose is exactly the "red no
                # commit can clear" the branch below names. That reasoning freed
                # `bench/` and `recordings/` and stopped one branch short of
                # here, because `_fixture_only` takes these paths first.
                problems.extend(
                    check_python_edit(repo, base, head, path, allow, inert_only=True)
                )
            elif path in PLAN_OWNED_STRUCTURAL_TESTS:
                problems.extend(_module_problems(repo, head, path))
            elif path.endswith(".py"):
                # Every `test/**/*.py` that is neither collected by pytest nor
                # imported by something that is. `test/pyproject.toml` sets
                # `testpaths = ["tests"]`, so `test/bench/**` and
                # `test/recordings/**` are separate trees with their own runners
                # — they assert nothing this guard's subject line claims to
                # protect. `_fixture_only` has already taken `test/src/**` and
                # `test/conftest.py` above, and those stay frozen precisely
                # because the collected tests import them.
                #
                # These trees are still oracles in their own right (pinned
                # latency floors, an `--expect-fail` gate, recorded terminal
                # output), so their *semantics* stay frozen by exactly the check
                # `test/tests/` gets. What they cannot be is frozen outright:
                # they name crate paths, and the crate split moves that tree
                # under them. A file both absolutely frozen and naming a moving
                # tree is a red no commit can clear, including the commit that
                # causes it — `--allow` cannot help, being line-scoped and
                # reachable only after a file passes this shape gate.
                #
                # Derived rather than enumerated: `test/bench/` was added as a
                # literal, and `test/recordings/` then reddened every merge for
                # a docstring line predating the rule. A third directory would
                # have been a third literal and a fourth ruling.
                problems.extend(
                    check_python_edit(
                        repo, base, head, path, allow,
                        sanctioned=sanctioned.get(path, frozenset()),
                        markers=bool(tiered) and path.startswith("test/tests/"),
                    )
                )
            else:
                problems.extend(check_relocation_only(repo, base, head, path, allow))
        else:
            problems.append(f"{path}: unexpected git status {status!r}")
    return problems


# ---------------------------------------------------------------------------
# Proof cases (scripts/tests/test_test_diff_guard.py): one throwaway repository per shape
# ---------------------------------------------------------------------------

_PYTEST_HEADER = "import pytest\n\n"

_BASE_TEST = (
    '"""Module docstring for crates/ocx_lib/src/thing.rs."""\n'
    + "import sys\n"
    + "from src import helpers\n"
    + _PYTEST_HEADER
    + "# a comment line\n"
    + "SCRIPT = '''\\\nline one\nline two\n'''\n"
    + "COUNT = 1\n"
    + "--COUNT\n"
    + "\n\n"
    + "def _helper(ocx):\n"
    + '    return ocx.run("about")\n'
    + "\n\n"
    + "def test_about(ocx):\n"
    + '    """Runs about."""\n'
    + "    result = _helper(ocx)\n"
    + "    assert result.returncode == 0\n"
    + "\n\n"
    + "def test_noted(ocx):\n"
    + '    """Runs about; the next two lines are prose, not code.\n'
    + "    assert _helper(ocx).returncode == 1\n"
    + '    and nothing else."""\n'
)

_BASE_TEST_NO_IMPORT = _BASE_TEST.replace(_PYTEST_HEADER, "")

# Line 852 is the constant; 851 and 853 are asserts, as in the real file —
# the allow-list must not carry them along.
_BASE_TRAMPOLINE = "".join(f"# pad {i}\n" for i in range(1, 851)) + (
    "assert SHIM_DIR.is_dir()\n"
    'BLOB = "crates/ocx_lib/src/shims/ocx-shim-x86_64.exe"\n'
    'assert BLOB.endswith(".exe")\n'
)
if not _BASE_TRAMPOLINE.splitlines()[851].startswith("BLOB ="):
    raise SystemExit("self-test fixture: the trampoline constant must be on line 852")

_BASE_ACCEPTANCE = "def test_patched(ocx):\n    assert ocx.run('about').returncode == 0\n"

_SRC_DOCSTRING = (
    '"""Fixture naming the wire shapes in `crates/ocx_lib/src/oci/index/wire.rs`."""\n'
    "\n"
    "SHAPES = (1, 2)\n"
)

_BASE_TREE: dict[str, str] = {
    "test/tests/test_a.py": _BASE_TEST,
    "test/tests/test_b.py": _BASE_TEST_NO_IMPORT,
    "test/tests/test_old.py": "def test_old():\n    assert True\n",
    "test/tests/test_reads_a_readme.py": (
        "def test_reads():\n    assert open('fixtures/pinned/README.md')\n"
    ),
    "test/tests/test_trampoline_exec.py": _BASE_TRAMPOLINE,
    "test/tests/test_smoke_coverage.py": "def test_every_verb():\n    assert VERBS == set()\n",
    "test/tests/test_patch_smoke.py": _BASE_ACCEPTANCE,
    "test/tests/conftest.py": "import pytest\n\n\n@pytest.fixture\ndef ocx():\n    return None\n",
    "test/conftest.py": "# session fixtures\n",
    "test/src/helpers.py": "def make_package():\n    return 1\n",
    "test/bench/shell_latency.py": _BASE_TEST,
    "test/recordings/test_truncate_digests.py": _BASE_TEST,
    "test/pyproject.toml": "[tool.pytest.ini_options]\nmarkers = []\n",
    "test/taskfile.yml": "version: '3'\n",
    "test/docker-compose.yml": "services: {}\n",
    # Two READMEs, so `README.md` alone identifies neither. One is named by a
    # collected module through its distinctive suffix; the other by nothing.
    "test/tests/fixtures/simplesigning/README.md": "see `crates/ocx_lib/src/a.rs`\n",
    "test/fixtures/pinned/README.md": "a fixture nothing imports\n",
    # A taskfile that INVOKES something under test/, which is what puts
    # `_invoked_by_a_taskfile` above its reader floor for every case. It lives
    # outside `test/` deliberately: `check_range` judges only `test/`, so the
    # fixture can carry a realistic invocation without `taskfile_adds_the_smoke_tier`
    # having to preserve it when it rewrites `test/taskfile.yml` wholesale.
    #
    # Its `desc:` names `inert_asset.json` and nothing else does. That is the
    # prose false positive `_command_text` must not fall for — and it is not
    # hypothetical, `test/taskfile.yml` being an ALLOWED_CONFIG file anyone may
    # add a line to.
    "taskfile.yml": (
        "version: '3'\n"
        "tasks:\n"
        "  drift:\n"
        "    desc: keeps fixtures/pinned/inert_asset.json in step with upstream\n"
        "    cmd: ./test/scripts/sync_thing.sh --check\n"
    ),
    "test/scripts/sync_thing.sh": '#!/bin/sh\ndest="crates/ocx_lib/tests/fixtures"\n',
    "test/fixtures/pinned/inert_asset.json": '{"src": "crates/ocx_lib/src/a.rs"}\n',
    "crates/ocx_lib/src/lib.rs": "pub fn x() {}\n",
    # C-007(e): UNIT_JUNIT lives under an ignored directory, as in the real
    # tree, so writing the report never makes the tree "dirty".
    ".gitignore": "/target/\n",
}

# ---- C-007 fixtures (--tiered-shapes) -------------------------------------
# A module of two tests and their helpers, the (a) move subject. Supplied per
# case through `Case.base` rather than `_BASE_TREE`, so no pre-existing case
# sees it. `_rows`, `ROWS` and `tomllib` are read by `test_rows_parse` alone —
# moving that test orphans exactly those three names.
_STRUCTURE_PATH = "test/tests/test_structure.py"
_STRUCTURE_ROWS_TEST = "def test_rows_parse():\n    assert isinstance(_rows(), dict)\n"
_STRUCTURE_ORPHANS = (
    "import tomllib\n",
    'ROWS = ROOT / "scoped_rows.toml"\n',
    '\n\ndef _rows():\n    return tomllib.loads(ROWS.read_text(encoding="utf-8"))\n',
)
_BASE_STRUCTURE = (
    '"""Structural checks over the repository tree."""\n'
    + _STRUCTURE_ORPHANS[0]
    + "from pathlib import Path\n"
    + "\n"
    + "ROOT = Path(__file__).parent.parent\n"
    + _STRUCTURE_ORPHANS[1]
    + "\n\n"
    + "def _read(name):\n"
    + '    return (ROOT / name).read_text(encoding="utf-8")\n'
    + _STRUCTURE_ORPHANS[2]
    + "\n\n"
    + "def test_floor_is_a_number():\n"
    + '    assert _read("SUITE_FLOOR").strip().isdigit()\n'
    + "\n\n"
    + _STRUCTURE_ROWS_TEST
)
# The M-module move: `test_rows_parse` leaves, its helpers stay behind.
_STRUCTURE_MINUS_ROWS_TEST = _BASE_STRUCTURE.replace("\n\n" + _STRUCTURE_ROWS_TEST, "", 1)
# The move with its orphans cleaned up: nothing left reads them.
_STRUCTURE_MINUS_ORPHANS = (
    _STRUCTURE_MINUS_ROWS_TEST.replace(_STRUCTURE_ORPHANS[0], "", 1)
    .replace(_STRUCTURE_ORPHANS[1], "", 1)
    .replace(_STRUCTURE_ORPHANS[2], "", 1)
)
if "_rows" in _STRUCTURE_MINUS_ORPHANS or "tomllib" in _STRUCTURE_MINUS_ORPHANS or "ROWS" in _STRUCTURE_MINUS_ORPHANS:
    raise SystemExit("self-test fixture: the orphan-cleanup module still names an orphan")
# The lint homes. Module-level statements differ from the source on purpose:
# the path constant is re-spelled and `_read` is local, as a real move reads.
_LINT_ROWS = (
    "import tomllib\n"
    "from pathlib import Path\n"
    "\n"
    'ROWS = Path(__file__).resolve().parents[1] / "scoped_rows.toml"\n'
    "\n\n"
    "def _rows():\n"
    '    return tomllib.loads(ROWS.read_text(encoding="utf-8"))\n'
    "\n\n"
    + _STRUCTURE_ROWS_TEST
)
_LINT_STRUCTURE = (
    "import tomllib\n"
    "from pathlib import Path\n"
    "\n"
    "ROOT = Path(__file__).resolve().parents[1]\n"
    'ROWS = ROOT / "scoped_rows.toml"\n'
    "\n\n"
    "def _read(name):\n"
    '    return (ROOT / name).read_text(encoding="utf-8")\n'
    "\n\n"
    "def _rows():\n"
    '    return tomllib.loads(ROWS.read_text(encoding="utf-8"))\n'
    "\n\n"
    "def test_floor_is_a_number():\n"
    '    assert _read("SUITE_FLOOR").strip().isdigit()\n'
    "\n\n"
    + _STRUCTURE_ROWS_TEST
)
# (c) bases: one module binding a single mark, one binding a list.
_SKIPIF = 'pytest.mark.skipif(sys.platform == "win32", reason="posix shell")'
_LOGIN_PATH = "test/tests/test_login.py"
_BASE_LOGIN = (
    "import sys\n\nimport pytest\n\n"
    f"pytestmark = {_SKIPIF}\n"
    "\n\n"
    'def test_login(ocx):\n    assert ocx.run("login").returncode == 0\n'
)
# A module whose marker lands below removed comments (c007c_marker_below_earlier_removals).
_SHIFT_PATH = "test/tests/test_shift.py"
# The acceptance build line C-021 re-spells under `--profile test-bin`.
_CARGO_TASKFILE = "version: '3'\ntasks:\n  build:\n    cmds:\n      - cargo build --release -p ocx --locked\n"
_RECONCILE_PATH = "test/tests/test_shell_reconcile.py"
_BASE_RECONCILE = (
    "import sys\n\nimport pytest\n\n"
    f"pytestmark = [{_SKIPIF}]\n"
    "\n\n"
    'def test_reconcile(ocx):\n    assert ocx.run("shell", "hook").returncode == 0\n'
)
# (e) the port of `test/tests/test_old.py::test_old` (a `_BASE_TREE` module
# holding exactly that one test) into `crates/ocx_lib/src/lib.rs`, whose
# libtest path is therefore `tests::old_is_true`.
_PORTED_LIB_RS = (
    "pub fn x() {}\n"
    "\n"
    "#[cfg(test)]\n"
    "mod tests {\n"
    "    // ported-from: test/tests/test_old.py::test_old\n"
    "    #[test]\n"
    "    fn old_is_true() {\n"
    "        assert!(true);\n"
    "    }\n"
    "}\n"
)
_PORTED_LIB_RS_IGNORED = _PORTED_LIB_RS.replace("    #[test]\n", "    #[test]\n    #[ignore]\n", 1)
_JUNIT_TARGET = "//crates/ocx_lib:ocx_lib_test"
_JUNIT_TEMPLATE = (
    f'<testsuites><testsuite name="{_JUNIT_TARGET}">'
    f'<testcase classname="{_JUNIT_TARGET}" name="{{name}}" time="0"{{tail}}'
    "</testsuite></testsuites>\n"
)
_JUNIT_PORT_PASSED = _JUNIT_TEMPLATE.format(name="tests::old_is_true", tail="/>")
_JUNIT_PORT_IGNORED = _JUNIT_TEMPLATE.format(
    name="tests::old_is_true", tail='><skipped message="ignored"/></testcase>'
)
# A report that executed something else: the port's name is absent.
_JUNIT_PORT_ABSENT = _JUNIT_TEMPLATE.format(name="tests::something_else", tail="/>")
# (b) content: C-008's conftest as the plan describes it — pytester, the
# fixture admission raised at setup, the subprocess wrapper, the budget hook.
_LINT_CONFTEST_C008 = (
    "import os\nimport shutil\nimport subprocess\nimport time\nfrom pathlib import Path\n\nimport pytest\n\n"
    'pytest_plugins = ["pytester"]\n\n'
    'FORBIDDEN = frozenset({"ocx", "ocx_binary", "registry", "mirror_registry", "legacy_registry",'
    ' "published_package", "sigstore_stack"})\n'
    'BIN = Path(__file__).resolve().parents[1] / "bin"\n'
    "_STARTED = time.monotonic()\n\n\n"
    "def pytest_runtest_setup(item):\n"
    '    banned = FORBIDDEN & set(getattr(item, "fixturenames", ()))\n'
    "    if banned:\n"
    '        raise pytest.UsageError(f"lint tier admits no fixture: {sorted(banned)}")\n\n\n'
    "@pytest.fixture(autouse=True)\n"
    "def _no_binaries(monkeypatch):\n"
    "    real = subprocess.Popen\n\n"
    "    def guarded(args, *rest, **kwargs):\n"
    "        exe = shutil.which(str(args[0])) or str(args[0])\n"
    '        if Path(exe).resolve().is_relative_to(BIN) or Path(exe).name == "task":\n'
    '            raise RuntimeError(f"lint tier admits no binary: {args[0]}")\n'
    "        return real(args, *rest, **kwargs)\n\n"
    '    monkeypatch.setattr(subprocess, "Popen", guarded)\n\n\n'
    "def pytest_sessionfinish(session, exitstatus):\n"
    '    if time.monotonic() - _STARTED > float(os.environ.get("OCX_LINT_BUDGET_SECONDS", "30")):\n'
    "        session.exitstatus = 1\n"
)
# A source module whose lint home carries it verbatim: a module-level skipif
# and a def the DEC-10(b) walk would refuse as new code (`subprocess.run`).
_SWEEP_PATH = "test/tests/test_sweep.py"
_BASE_SWEEP = (
    "import subprocess\nimport sys\n\nimport pytest\n\n"
    'pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="posix paths")\n\n\n'
    "def test_sweep():\n"
    '    out = subprocess.run(["git", "ls-files"], capture_output=True, text=True, check=True).stdout\n'
    "    assert out\n"
)
# The same spawn in an undecorated helper rather than the test: carried when
# it arrives verbatim, walked as new code when its body changed on the way.
_SWEEP_HELPER_PATH = "test/tests/test_sweep_helper.py"
_BASE_SWEEP_HELPER = (
    "import subprocess\n\n\n"
    "def _tracked():\n"
    '    return subprocess.run(["git", "ls-files"], capture_output=True, text=True, check=True).stdout\n'
    "\n\n"
    "def test_tree_is_tracked():\n"
    "    assert _tracked()\n"
)
_AUTOUSE_PATH = "test/tests/test_p3.py"
_BASE_AUTOUSE = (
    "from src.helpers import leak_check  # noqa: F401\n\n\n"
    "def test_keep():\n    assert 1\n\n\ndef test_moved():\n    assert 2\n"
)
_CLASS_DECO_PATH = "test/tests/test_p5.py"
_BASE_CLASS_DECO = (
    'import pytest\n\n\n@pytest.mark.usefixtures("check")\n'
    "class TestX:\n    def test_m(self):\n        assert 1\n"
)
# Two modules, one identical test each; one lint copy sanctions only one of them.
_DUP1_PATH = "test/tests/test_dup1.py"
_DUP2_PATH = "test/tests/test_dup2.py"
_BASE_DUP = "def test_same():\n    assert 1\n"
_AA_PATH = "test/tests/test_aa.py"
_BB_PATH = "test/tests/test_bb.py"
_BASE_AA = "def test_a():\n    assert 1\n"
_BB_SKIPIF = 'import sys\n\nimport pytest\n\npytestmark = pytest.mark.skipif(sys.platform != "darwin", reason="mac")\n'
_BASE_BB = _BB_SKIPIF + "\n\ndef test_b():\n    assert 2\n"
_LINT_AA_WITH_BB_SKIPIF = _BB_SKIPIF + "\n\n" + _BASE_AA
# A moved test whose body is identical but whose module constant is not.
_CASES_PATH = "test/tests/test_cases.py"
_BASE_CASES = (
    "CASES = [0, False]\n\n\n"
    "def _each():\n    return list(CASES)\n\n\n"
    "def test_check():\n    for value in _each():\n        assert value\n"
)
_CLS_PATH = "test/tests/test_cls.py"
_BASE_CLS = (
    "def test_keep():\n    assert True\n\n\n"
    "class TestOld:\n    def test_m(self):\n        assert 1 == 1\n"
)
_CLS_MINUS_CLASS = "def test_keep():\n    assert True\n"
_PORTED_METHOD_LIB_RS = _PORTED_LIB_RS.replace(
    "test/tests/test_old.py::test_old", "test/tests/test_cls.py::TestOld::test_m"
).replace("fn old_is_true", "fn old_method_is_true")
_PORT_HEAD: dict[str, str | None] = {
    "test/tests/test_old.py": None,
    "crates/ocx_lib/src/lib.rs": _PORTED_LIB_RS,
}


@dataclass(slots=True)
class Case:
    name: str
    expect_red: bool
    head: dict[str, str | None]  # path → new content, None = delete
    allow: list[str] = field(default_factory=list)  # `--allow` specs, PATH:LINE
    # Files the case needs in the BASE commit, over `_BASE_TREE` — a removal
    # can only be shown against a base that carried the thing removed.
    base: dict[str, str] = field(default_factory=dict)
    # --tiered-shapes (C-007). The case runs under the flag, with fakes for
    # `task bazel:test:unit` (writes `junit` to UNIT_JUNIT unless None, returns
    # `unit_exit`) and for pytest collection (one case per node id).
    tiered: bool = False
    junit: str | None = None
    unit_exit: int = 0
    # A report already on disk before the run, written moments earlier (same
    # second, never backdated): the fake runner leaving it in place is the
    # stale-report shape a rounded mtime check let through.
    stale_junit: str | None = None
    # HEAD when the guard runs: "tip" (the head commit itself), "merge" (a
    # `--no-ff` merge of the tip into base's branch, P-5), "behind" (base
    # checked out, so the tip is no ancestor of HEAD), "dirty" (an untracked,
    # non-ignored file beside the tip).
    head_state: str = "tip"
    # A tiered green whose shape the guard already admits without the flag
    # gets no `__flag_off` red twin.
    twin: bool = True


def _edit(text: str, old: str, new: str) -> str:
    if old not in text:
        raise SystemExit(f"self-test fixture: edit target {old!r} not in text")
    return text.replace(old, new, 1)


SELF_TEST_CASES: list[Case] = [
    # ---- allowed shapes → green ---------------------------------------
    Case("marker_added", False, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "def test_about", "@pytest.mark.smoke\ndef test_about"),
    }),
    Case("marker_added_with_import_pytest", False, {
        # The import lands below the module docstring, as in every real
        # module — above it, the docstring would stop being one.
        "test/tests/test_b.py": _edit(_BASE_TEST, "def test_about", "@pytest.mark.smoke\ndef test_about"),
    }),
    Case("xdist_group_mark_added", False, {
        # Scheduling, not assertion: the test is pinned to the worker that
        # already owns the registry-wide slot it writes.
        "test/tests/test_a.py": _edit(
            _BASE_TEST, "def test_about", '@pytest.mark.xdist_group("patch_global_slot")\ndef test_about'
        ),
    }),
    Case("docstring_path_respelled", False, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "crates/ocx_lib/src/thing.rs", "crates/ocx_store/src/thing.rs"),
    }),
    Case("comment_line_changed", False, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "# a comment line", "# a reworded comment line"),
    }),
    # ---- test/bench/**: same route as test/tests/**, both polarities ----
    Case("bench_docstring_path_respelled", False, {
        "test/bench/shell_latency.py": _edit(_BASE_TEST, "crates/ocx_lib/src/thing.rs", "crates/ocx_store/src/thing.rs"),
    }),
    Case("bench_comment_line_changed", False, {
        "test/bench/shell_latency.py": _edit(_BASE_TEST, "# a comment line", "# a reworded comment line"),
    }),
    # The route is shape-only: it does not make the bench's numbers editable.
    # Without this case the widening above would be indistinguishable from
    # opening `test/bench/**` outright.
    Case("bench_constant_changed", True, {
        "test/bench/shell_latency.py": _edit(_BASE_TEST, "COUNT = 1", "COUNT = 2"),
    }),
    # A non-.py file under test/bench/ still falls to the catch-all — the
    # route keys on the extension, not on the directory alone.
    Case("bench_data_file_changed", True, {
        "test/bench/results.json": '{"ms": 2}\n',
    }, base={"test/bench/results.json": '{"ms": 1}\n'}),
    # `test/recordings/**` is the third uncollected tree, and the reason the
    # route is derived from the extension rather than listed per directory:
    # adding `test/bench/` as a literal left this one reddening every merge for
    # a docstring line that predated the rule.
    Case("recordings_docstring_path_respelled", False, {
        "test/recordings/test_truncate_digests.py": _edit(
            _BASE_TEST, "crates/ocx_lib/src/thing.rs", "crates/ocx_index/src/thing.rs"
        ),
    }),
    Case("recordings_constant_changed", True, {
        "test/recordings/test_truncate_digests.py": _edit(_BASE_TEST, "COUNT = 1", "COUNT = 3"),
    }),
    Case("blank_line_added_between_functions", False, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "\n\ndef test_about", "\n\n\ndef test_about"),
    }),
    Case("new_test_function_appended", False, {
        "test/tests/test_a.py": _BASE_TEST
        + "\n\n@pytest.mark.smoke\ndef test_version(ocx):\n"
        + '    assert ocx.run("version").returncode == 0\n',
    }),
    Case("new_test_file", False, {
        "test/tests/test_new.py": "import pytest\n\n\n@pytest.fixture\ndef thing():\n    return 1\n\n\n"
        + '@pytest.mark.parametrize("n", [1, 2])\ndef test_new(thing, n):\n    assert thing < n + 1\n',
    }),
    Case("new_test_with_fixtures_monkeypatch_and_marks", False, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\n@pytest.mark.smoke\n@pytest.mark.parametrize("n", [1])\n'
        + "def test_env(ocx, monkeypatch, tmp_path, n):\n"
        + '    monkeypatch.setenv("OCX_HOME", str(tmp_path))\n'
        + '    assert ocx.run("about").returncode == n - 1\n',
    }),
    Case("new_test_class_with_methods", False, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\nclass TestAbout:\n    """Grouped."""\n\n'
        + "    @pytest.mark.smoke\n    def test_a(self, ocx):\n"
        + '        assert ocx.run("about").returncode == 0\n',
    }),
    Case("plan_owned_structural_test_assert_edited", False, {
        "test/tests/test_smoke_coverage.py": "def test_every_verb():\n    assert VERBS == {'pull'}\n",
    }),
    Case("config_and_floor_files", False, {
        # `-n`, `--dist`, `--maxfail`, `-v` are not selection tokens.
        "test/pyproject.toml": "[tool.pytest.ini_options]\nmarkers = ['smoke']\naddopts = '--strict-markers --maxfail=3'\n",
        "test/taskfile.yml": "version: '3'\ntasks:\n  smoke:\n    cmd: uv run pytest -n auto --dist loadgroup -v\n",
        "test/SUITE_FLOOR": "3618\n",
    }),
    Case("allow_listed_trampoline_hunk", False, {
        "test/tests/test_trampoline_exec.py": _edit(
            _BASE_TRAMPOLINE, "crates/ocx_lib/src/shims/", "crates/ocx_shim_blobs/src/shims/"
        ),
    }, allow=["test/tests/test_trampoline_exec.py:852"]),
    Case("changes_outside_test_tree", False, {
        "crates/ocx_lib/src/lib.rs": None,
        "crates/ocx_store/src/lib.rs": "pub fn x() {}\n",
    }),
    # ---- forbidden shapes → red ---------------------------------------
    Case("test_file_deleted", True, {"test/tests/test_old.py": None}),
    Case("test_file_renamed", True, {
        "test/tests/test_old.py": None,
        "test/tests/test_older.py": "def test_old():\n    assert True\n",
    }),
    Case("assert_line_removed", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "    assert result.returncode == 0\n", ""),
    }),
    Case("assert_line_changed", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "returncode == 0", "returncode in (0, 1)"),
    }),
    Case("skip_marker_added", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "def test_about", '@pytest.mark.skip(reason="flaky")\ndef test_about'),
    }),
    Case("xfail_marker_added", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "def test_about", "@pytest.mark.xfail\ndef test_about"),
    }),
    Case("parametrize_added", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "def test_about", '@pytest.mark.parametrize("n", [1])\ndef test_about'),
    }),
    Case("fixture_signature_changed", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "def test_about(ocx):", "def test_about(ocx, tmp_path):"),
    }),
    Case("helper_body_changed", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, 'ocx.run("about")', 'ocx.run("version")'),
    }),
    Case("new_fixture_shadows_conftest", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\n@pytest.fixture\ndef ocx():\n    return None\n",
    }),
    Case("new_function_shadows_existing_test", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_about(ocx):\n    pass\n",
    }),
    Case("new_helper_is_not_a_test", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef _other(ocx):\n    return 1\n",
    }),
    Case("new_pytest_hook", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef pytest_generate_tests(metafunc):\n    pass\n",
    }),
    Case("pytestmark_added", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "# a comment line", "pytestmark = pytest.mark.smoke\n# a comment line"),
    }),
    Case("marker_variant_is_not_exact", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "def test_about", "@pytest.mark.smoke(1)\ndef test_about"),
    }),
    Case("xdist_group_from_a_name_is_not_greppable", True, {
        # A group built at runtime hides which slot is joined, which is the
        # one thing the literal spelling buys.
        "test/tests/test_a.py": _edit(
            _BASE_TEST, "def test_about", "@pytest.mark.xdist_group(SLOT)\ndef test_about"
        ),
    }),
    Case("xdist_group_skip_wearing_the_mark_shape", True, {
        # The allowance is the mark, not the decorator column: a skip added
        # beside it is still a silenced test.
        "test/tests/test_a.py": _edit(
            _BASE_TEST, "def test_about", '@pytest.mark.skip("later")\ndef test_about'
        ),
    }),
    Case("other_import_added", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "import pytest\n", "import os\nimport pytest\n"),
    }),
    Case("docstring_extended_over_body", True, {
        # `-"""Runs about."""` / `+"""Runs about.` / `+    """` — every
        # changed line is a docstring line on its own side; the body between
        # is untouched per git and is now text.
        "test/tests/test_a.py": _edit(
            _BASE_TEST,
            '    """Runs about."""\n    result = _helper(ocx)\n    assert result.returncode == 0\n',
            '    """Runs about.\n    result = _helper(ocx)\n    assert result.returncode == 0\n    """\n',
        ),
    }),
    Case("docstring_shrunk_over_body", True, {
        # The inverse: the docstring closes one line earlier and its former
        # interior — untouched per git — is now an executable `assert`.
        "test/tests/test_a.py": _edit(
            _BASE_TEST,
            '    """Runs about; the next two lines are prose, not code.\n'
            "    assert _helper(ocx).returncode == 1\n"
            '    and nothing else."""\n',
            '    """Runs about; the next two lines are prose, not code."""\n'
            "    assert _helper(ocx).returncode == 1\n"
            "    # and nothing else.\n",
        ),
    }),
    Case("added_line_starts_with_plus_plus", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "# a comment line\n", "# a comment line\n++print('sabotage')\n"),
    }),
    Case("removed_line_starts_with_minus_minus", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "--COUNT\n", ""),
    }),
    Case("new_pytest_ini", True, {
        "test/pytest.ini": "[pytest]\naddopts = --deselect tests/test_a.py::test_about\n",
    }),
    Case("new_src_module", True, {
        "test/src/extra.py": "def helper():\n    return 1\n",
    }),
    Case("new_tests_init_py", True, {
        "test/tests/__init__.py": "",
    }),
    Case("docstring_gains_assert", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, '"""Runs about."""', '"""Runs about and assert nothing."""'),
    }),
    Case("blank_line_inside_string_constant", True, {
        "test/tests/test_a.py": _edit(_BASE_TEST, "line one\nline two", "line one\n\nline two"),
    }),
    Case("src_helper_edited", True, {
        "test/src/helpers.py": "def make_package():\n    return 2\n",
    }),
    # --- the three routes WP-28 forced, each shown both ways -----------------
    # A `test/src/**` docstring naming a tree the split moves. Frozen bytes made
    # this a red no commit could clear: reverting it manufactures a dead path the
    # sweep then reds on, so the two instruments contradicted each other here.
    Case("src_docstring_repointed", False, {
        "test/src/static_index.py": _SRC_DOCSTRING.replace(
            "ocx_lib/src/oci/index/wire.rs", "ocx_index/src/wire.rs"
        ),
    }, base={"test/src/static_index.py": _SRC_DOCSTRING}),
    # Semantics stay frozen: the value a collected test imports is not prose.
    Case("src_constant_changed_under_docstring", True, {
        "test/src/static_index.py": _SRC_DOCSTRING.replace("SHAPES = (1, 2)", "SHAPES = (1, 3)"),
    }, base={"test/src/static_index.py": _SRC_DOCSTRING}),
    # `inert_only` drops the additive routes: a new `def` in a fixture module is
    # new fixture behaviour arriving under the permit written for new tests.
    Case("src_new_def_added", True, {
        "test/src/static_index.py": _SRC_DOCSTRING + "\n\ndef helper():\n    return 1\n",
    }, base={"test/src/static_index.py": _SRC_DOCSTRING}),
    # The discriminating case for `inert_only`. `import pytest` is permitted
    # unconditionally in a collected module — it is what a module needs before
    # it can carry a smoke marker. In a fixture module nothing is collected, so
    # the same line is a new dependency wearing the permit written for markers.
    # This shape reds ONLY because the additive routes are switched off; a plain
    # `def` proved nothing, being already refused by `new_definition_lines`
    # (DEC-61: a red through another guard is not evidence).
    Case("src_bare_pytest_import_added", True, {
        "test/src/static_index.py": _SRC_DOCSTRING + "import pytest\n",
    }, base={"test/src/static_index.py": _SRC_DOCSTRING}),
    # The discriminating case for the derived route. `docker-compose.yml` is
    # named by `conftest.py`, so the suite RUNS it, and an allow-list entry must
    # not reach it. Under DEC-65's first spelling this was green: the route
    # keyed on "not Python", and a one-for-one line in the compose file — a
    # registry image, a port — was one `--allow` away from passing. The existing
    # `other_test_asset_edited` case cannot show this, because it carries no
    # allow and so reds either way (DEC-61).
    Case("asset_named_by_the_suite_refused_despite_allow", True, {
        "test/docker-compose.yml": "services: {registry: {image: registry:3}}\n",
    }, allow=["test/docker-compose.yml:1"],
       # Named ONLY from a comment, as the real `test/conftest.py` does — its
       # code lines say "docker-compose" without the extension. The first
       # fixture here put it on a code line, which made this case pass for a
       # reason the real tree does not share and hid a live regression.
       base={"test/conftest.py": "# session fixtures; port knobs docker-compose.yml binds\n"}),
    # The needle must be UNAMBIGUOUS, not merely present. Ten files under
    # `test/` are called `README.md`, so a bare-basename needle answered True
    # for every one of them in every state — constant-True, which is the defect
    # this whole route exists to refuse, reintroduced one level down. This
    # README is named by nothing, so it takes the asset route.
    Case("unnamed_readme_takes_the_asset_route", False, {
        "test/tests/fixtures/simplesigning/README.md": "see `crates/ocx_sign/src/a.rs`\n",
    }, allow=["test/tests/fixtures/simplesigning/README.md:1"]),
    # Markdown is not a free-for-all. It reaches the relocation route, which
    # permits exactly one allow-listed one-for-one line — so a README nobody
    # allow-listed still reds, and so does a rewrite that adds a line.
    Case("readme_without_an_allow_still_reds", True, {
        "test/fixtures/pinned/README.md": "a fixture nothing imports, reworded\n",
    }),
    Case("readme_rewrite_reds_despite_an_allow", True, {
        "test/fixtures/pinned/README.md": "reworded\nand a second line\n",
    }, allow=["test/fixtures/pinned/README.md:1"]),
    # An asset the suite neither collects, imports nor runs: one-for-one only.
    Case("test_asset_one_for_one_allowed", False, {
        "test/scripts/vendor.sh": '#!/bin/sh\ndest="crates/ocx_index/tests/fixtures"\n',
    }, allow=["test/scripts/vendor.sh:2"],
       base={"test/scripts/vendor.sh": '#!/bin/sh\ndest="crates/ocx_lib/tests/fixtures"\n'}),
    # The same asset, same allow-list, two lines for one — the DEC-10 (d) permit
    # is a re-point, and a re-point is never a rewrite.
    Case("test_asset_multiline_refused_despite_allow", True, {
        "test/scripts/vendor.sh": '#!/bin/sh\ndest="crates/ocx_index/tests/fixtures"\nrm -rf "$dest"\n',
    }, allow=["test/scripts/vendor.sh:2"],
       base={"test/scripts/vendor.sh": '#!/bin/sh\ndest="crates/ocx_lib/tests/fixtures"\n'}),
    # ---- a test/ file a taskfile runs is tooling, not an inert asset ----
    # `vendor.sh` above is invoked by nothing and keeps the relocation rule;
    # `sync_thing.sh` is invoked by the fixture's root taskfile and may be
    # maintained. Both live in `test/scripts/`, so the discriminator is the
    # invocation and not the directory.
    Case("taskfile_invoked_script_may_be_rewritten", False, {
        "test/scripts/sync_thing.sh": (
            '#!/bin/sh\nset -eu\ndest="crates/ocx_index/tests/fixtures"\n'
            'mkdir -p "$dest"\nrsync -a upstream/ "$dest"\n'
        ),
    }),
    # The same rewrite on a file whose ONLY mention in a taskfile is a `desc:`.
    # Prose is not invocation; without that rule a permitted one-line edit to a
    # taskfile would free any asset it names.
    # Allow-listed AND a rewrite, so the red can only be the relocation rule:
    # were prose counted as invocation this file would take the free route and
    # the case would be green.
    Case("asset_named_only_in_taskfile_prose_is_not_invoked", True, {
        "test/fixtures/pinned/inert_asset.json": '{"src": "crates/ocx_index/src/a.rs"}\n{"added": true}\n',
    }, allow=["test/fixtures/pinned/inert_asset.json:1"]),
    # And the ordering: a task running it does not thaw a file the collected
    # suite names. DEC-69's hole (`docker-compose.yml`) stays closed.
    Case("taskfile_invoked_script_named_by_a_collected_module_stays_frozen", True, {
        "test/scripts/sync_thing.sh": '#!/bin/sh\ndest="crates/ocx_index/tests/fixtures"\n',
    }, base={
        "test/tests/test_reads_sync.py": (
            "def test_reads():\n    assert open('scripts/sync_thing.sh')\n"
        ),
    }),
    # Unchanged: an asset edit nobody allow-listed still reds.
    Case("session_conftest_edited", True, {
        "test/conftest.py": "# session fixtures, reworded\n",
    }),
    Case("tests_conftest_fixture_changed", True, {
        "test/tests/conftest.py": "import pytest\n\n\n@pytest.fixture\ndef ocx():\n    return 1\n",
    }),
    Case("new_conftest_added", True, {
        "test/tests/sub/conftest.py": "import pytest\n",
    }),
    Case("other_test_asset_edited", True, {
        "test/docker-compose.yml": "services: {registry: {}}\n",
    }),
    Case("allow_list_names_the_wrong_line", True, {
        "test/tests/test_trampoline_exec.py": _edit(
            _BASE_TRAMPOLINE, "crates/ocx_lib/src/shims/", "crates/ocx_shim_blobs/src/shims/"
        ),
    }, allow=["test/tests/test_trampoline_exec.py:12"]),
    Case("allow_listed_hunk_spans_the_neighbouring_assert", True, {
        # 852 re-pointed and 853 neutralised: one `--unified=0` hunk of two
        # lines, which the allow-list must not carry.
        "test/tests/test_trampoline_exec.py": _edit(
            _edit(_BASE_TRAMPOLINE, "crates/ocx_lib/src/shims/", "crates/ocx_shim_blobs/src/shims/"),
            'assert BLOB.endswith(".exe")',
            "assert True",
        ),
    }, allow=["test/tests/test_trampoline_exec.py:852"]),
    Case("in_series_acceptance_test_assert_edited", True, {
        "test/tests/test_patch_smoke.py": _BASE_ACCEPTANCE.replace("== 0", "in (0, 1)"),
    }),
    Case("new_class_is_not_test_named", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\nclass Helper:\n    def test_x(self):\n        pass\n",
    }),
    Case("new_test_class_body_rebinds_a_global", True, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\nclass TestX:\n    globals()["_helper"] = lambda ocx: None\n\n'
        + "    def test_x(self):\n        pass\n",
    }),
    Case("new_test_with_default_argument", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx, patched=setattr(sys, 'x', 1)):\n    pass\n",
    }),
    Case("new_test_with_foreign_decorator", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\n@_helper\ndef test_x(ocx):\n    pass\n",
    }),
    Case("new_test_declares_global", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    global COUNT\n    COUNT = 2\n",
    }),
    Case("new_test_calls_globals", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    globals()["_helper"] = None\n',
    }),
    Case("new_test_calls_setattr", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    setattr(helpers, "make_package", None)\n',
    }),
    Case("new_test_touches_sys_modules", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    sys.modules["src.helpers"] = None\n',
    }),
    Case("new_test_stores_to_an_imported_module", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    helpers.make_package = lambda: 2\n",
    }),
    # ---- H2: the review adversary's executed bypass and its siblings ------
    Case("adversary_new_test_writes_through___globals__", True, {
        # The exact shape that flipped an unchanged assertion: a new def, no
        # `globals(` call, no attribute store — a subscript store into the
        # dict a function's `__globals__` hands out.
        "test/tests/test_a.py": _BASE_TEST
        + '\n\ndef test_x(ocx):\n    test_about.__globals__["_helper"] = lambda ocx: None\n',
    }),
    Case("new_test_writes_through___dict__", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    helpers.__dict__["make_package"] = None\n',
    }),
    Case("new_test_calls_vars", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    vars(helpers)["make_package"] = None\n',
    }),
    Case("new_test_subscript_store_into_an_imported_module", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    helpers.registry["make_package"] = None\n',
    }),
    Case("new_test_reads_through_imports", False, {
        # Reads through an import, attribute and subscript alike, are what
        # every ordinary test does; only a Store/Del context is a rebinding.
        "test/tests/test_a.py": _BASE_TEST
        + "\n\ndef test_x(ocx):\n    assert helpers.make_package() == 1\n    assert sys.argv[0] != helpers.__name__\n",
    }),
    # ---- W3: a new module is parsed, not accepted by name -----------------
    Case("new_file_stores_into_an_imported_module", True, {
        "test/tests/test_new.py": "from src import helpers\n\n\ndef test_x():\n    helpers.make_package = lambda: 2\n",
    }),
    Case("new_file_rebinds_at_module_level", True, {
        "test/tests/test_new.py": 'import sys\n\nsys.modules["src.helpers"] = None\n\n\ndef test_x():\n    pass\n',
    }),
    Case("new_file_with_helpers_and_module_local_fixture", False, {
        "test/tests/test_new.py": "import pytest\nfrom src import helpers\n\n\ndef _twice(n):\n    return n * 2\n\n\n"
        + "@pytest.fixture\ndef thing():\n    return helpers.make_package()\n\n\n"
        + "def test_new(thing):\n    assert _twice(thing) == 2\n",
    }),
    Case("plan_owned_structural_test_rebinds_a_module", True, {
        "test/tests/test_smoke_coverage.py": 'import sys\nsys.modules["x"] = None\n\n\ndef test_every_verb():\n    assert VERBS == set()\n',
    }),
    # ---- W4: ALLOWED_CONFIG may not add a selection token ----------------
    Case("config_adds_deselect", True, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\naddopts = '--strict-markers --deselect tests/test_a.py::test_about'\n",
    }),
    Case("config_adds_ignore", True, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\naddopts = '--ignore=tests/test_a.py'\n",
    }),
    Case("taskfile_adds_k_expression", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  quick:\n    cmd: uv run pytest -k 'not about'\n",
    }),
    Case("taskfile_adds_m_expression", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  quick:\n    cmd: uv run pytest -m 'not smoke'\n",
    }),
    Case("taskfile_narrows_the_smoke_tier", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  smoke:\n    cmd: uv run pytest -m \"smoke and not slow\" -n auto\n",
    }),
    # ---- L1 review: bypasses executed under real pytest -----------------
    Case("new_test_aliases_an_imported_module", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    h = helpers\n    h.make_package = lambda: 2\n",
    }),
    Case("new_test_aliases_via_walrus", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    (h := helpers).make_package = lambda: 2\n",
    }),
    Case("new_test_imports_locally_then_stores", True, {
        "test/tests/test_a.py": _BASE_TEST
        + "\n\ndef test_x(ocx):\n    from src import helpers as h\n    h.make_package = lambda: 2\n",
    }),
    Case("new_test_imports_sys_modules_locally", True, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\ndef test_x(ocx):\n    from sys import modules\n    modules["src.helpers"] = None\n',
    }),
    Case("new_test_calls_dunder_setattr", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    helpers.__setattr__("make_package", None)\n',
    }),
    Case("new_test_calls_object_dunder_setattr", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    object.__setattr__(helpers, "make_package", None)\n',
    }),
    Case("new_test_getattr_reaches___dict__", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    getattr(helpers, "__dict__")["make_package"] = None\n',
    }),
    Case("new_test_calls___getattribute__", True, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\ndef test_x(ocx):\n    helpers.__getattribute__("__dict__")["make_package"] = None\n',
    }),
    Case("new_test_aliases_globals", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    g = globals\n    g()["_helper"] = None\n',
    }),
    Case("new_test_aliases_setattr", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    s = setattr\n    s(helpers, "make_package", None)\n',
    }),
    Case("new_test_subscripts___builtins__", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    __builtins__["exec"]("_helper = None")\n',
    }),
    Case("new_test_calls_builtins_exec", True, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\ndef test_x(ocx):\n    import builtins\n    builtins.exec("_helper = None")\n',
    }),
    Case("new_test_persisting_monkeypatch", True, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\ndef test_x(ocx):\n    pytest.MonkeyPatch().setattr(helpers, "make_package", lambda: 2)\n',
    }),
    Case("new_test_started_mock_patch", True, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\ndef test_x(ocx):\n    from unittest import mock\n    mock.patch.object(helpers, "make_package").start()\n',
    }),
    Case("new_test_import_module_store", True, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\ndef test_x(ocx):\n    import importlib\n    importlib.import_module("src.helpers").make_package = None\n',
    }),
    Case("new_test_reloads_a_module", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    import importlib\n    importlib.reload(helpers)\n",
    }),
    Case("new_test_class_with_decorator", True, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\n@(lambda c: (setattr(helpers, "make_package", None), c)[1])\nclass TestX:\n    def test_x(self):\n        pass\n',
    }),
    Case("new_test_class_with_base", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\nclass TestX(helpers.Base):\n    def test_x(self):\n        pass\n",
    }),
    Case("new_test_class_with_metaclass", True, {
        "test/tests/test_a.py": _BASE_TEST
        + '\n\nclass TestX(metaclass=lambda n, b, d: setattr(helpers, "make_package", None) or type(n, b, d)):\n'
        + "    def test_x(self):\n        pass\n",
    }),
    Case("new_file_declares_pytest_plugins", True, {
        "test/tests/test_new.py": 'pytest_plugins = ["src.evil"]\n\n\ndef test_x():\n    pass\n',
    }),
    Case("new_file_class_with_metaclass", True, {
        "test/tests/test_new.py": "from src import helpers\n\n\nclass TestX(metaclass=helpers.Meta):\n    def test_x(self):\n        pass\n",
    }),
    Case("new_test_ordinary_shapes_still_pass", False, {
        # `monkeypatch.setenv`, a local alias of a fixture value, `__name__`,
        # `.get(`/`.pop(` on a local dict: none of these reach a module.
        "test/tests/test_a.py": _BASE_TEST
        + "\n\n@pytest.mark.smoke\ndef test_x(ocx, monkeypatch, tmp_path):\n"
        + '    monkeypatch.setenv("OCX_HOME", str(tmp_path))\n    home = tmp_path\n    seen = {"a": 1}\n'
        + '    assert seen.pop("a") == 1 and home.name and helpers.__name__ == "src.helpers"\n',
    }),
    # ---- DEC-10(b) strict (owner ruling 2026-09-16): stores only into own names,
    # monkeypatch the one mutation route ----------------------------------------
    Case("new_test_stores_into_os_environ", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    import os\n    os.environ["X"] = "1"\n',
    }),
    Case("new_test_stores_into_module_level_dict", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    REGISTRY["k"] = ocx\n',
    }),
    Case("new_test_stores_into_imported_table", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    helpers.TABLE["k"] = ocx\n',
    }),
    Case("new_test_augments_a_module_global", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    COUNT += 1\n",
    }),
    Case("new_test_updates_module_level_dict", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    REGISTRY.update({"k": 1})\n',
    }),
    Case("new_test_appends_to_sys_path", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx, tmp_path):\n    sys.path.append(str(tmp_path))\n',
    }),
    Case("new_test_clears_an_imported_cache", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    helpers.CACHE.clear()\n",
    }),
    Case("new_test_clears_through_a_local_alias", True, {
        # `c` is bound, but to a reference into helpers — the same object.
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    c = helpers.CACHE\n    c.clear()\n",
    }),
    Case("new_test_stores_through_a_local_alias", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    r = REGISTRY\n    r["k"] = 1\n',
    }),
    Case("new_test_mutates_another_fixture", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    ocx.env.update({"X": "1"})\n',
    }),
    Case("new_test_mutates_a_call_result_of_a_module", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    helpers.registry().update({"k": 1})\n',
    }),
    Case("new_test_deletes_a_module_global", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    del COUNT\n",
    }),
    Case("new_test_declares_nonlocal", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    def inner():\n        nonlocal ocx\n        ocx = None\n    inner()\n",
    }),
    Case("new_file_def_stores_into_its_own_module_global", True, {
        # A new module may hold state, but a def in it may not store into it.
        "test/tests/test_new.py": "SEEN = {}\n\n\ndef test_x():\n    SEEN['k'] = 1\n",
    }),
    Case("new_test_own_bindings_and_monkeypatch", False, {
        "test/tests/test_a.py": _BASE_TEST
        + "\n\ndef test_x(ocx, monkeypatch, tmp_path):\n"
        + '    d = {}\n    d["k"] = 1\n    d.update({"j": 2})\n    seen = set()\n    seen.add(1)\n    n = 0\n    n += 1\n'
        + '    monkeypatch.setenv("X", "1")\n    monkeypatch.setattr(helpers, "make_package", lambda: 2)\n'
        + '    monkeypatch.setitem(REGISTRY, "k", 1)\n    with monkeypatch.context() as m:\n'
        + '        m.setattr(helpers, "make_package", lambda: 3)\n'
        + '    for k in helpers.TABLE:\n        seen.add(k)\n    items = [x for x in sys.argv if x]\n'
        + '    result = helpers.make_package()\n    path = tmp_path.joinpath("x")\n    a, b = 1, 2\n'
        + '    with open(path, "w", encoding="utf-8") as fh:\n        fh.write("x")\n'
        + "    assert result and path and (a, b) and d and n and items is not None\n",
    }),
    # ---- W4 spellings probed green by the L1 review ------------------------
    Case("config_sets_testpaths", True, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\ntestpaths = ['tests/subset']\n",
    }),
    Case("config_sets_python_files", True, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\npython_files = 'test_a*.py'\n",
    }),
    Case("config_sets_python_functions", True, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\npython_functions = 'test_about*'\n",
    }),
    Case("config_sets_python_classes", True, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\npython_classes = 'Nope*'\n",
    }),
    Case("taskfile_adds_collect_only", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest --collect-only -n auto\n",
    }),
    Case("taskfile_adds_co", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest --co -q\n",
    }),
    Case("taskfile_adds_override_ini_short", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest -o testpaths=tests/subset\n",
    }),
    Case("taskfile_adds_override_ini_long", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest --override-ini addopts=''\n",
    }),
    Case("taskfile_adds_config_file", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest -c alt.ini\n",
    }),
    Case("taskfile_adds_noconftest", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest --noconftest\n",
    }),
    Case("taskfile_adds_last_failed", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest --lf\n",
    }),
    Case("taskfile_adds_failed_first", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest --ff -n auto\n",
    }),
    Case("taskfile_adds_stepwise", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest --sw\n",
    }),
    Case("taskfile_adds_glued_k_expression", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  quick:\n    cmd: uv run pytest -k'not about'\n",
    }),
    Case("taskfile_shell_idioms_and_floor_collect", False, {
        # `set -o pipefail`, `sh -c '…'` and the floor's `--collect-only -q`
        # are the series' own lines; none selects a test.
        "test/taskfile.yml": "version: '3'\ntasks:\n  floor:\n    cmd: >-\n      set -o pipefail;\n"
        + "      sh -c 'echo x';\n      summary=$(uv run pytest --collect-only -q --color=no 2>&1 | tail -n 1)\n",
    }),
    # ---- L2 review: import time is a scope too ---------------------------
    Case("new_file_updates_os_environ_at_import", True, {
        "test/tests/test_new.py": 'import os\n\nos.environ.update({"OCX_HOME": "/tmp/shadow"})\n\n\ndef test_x():\n    pass\n',
    }),
    Case("new_file_inserts_into_sys_path_at_import", True, {
        "test/tests/test_new.py": 'import sys\n\nsys.path.insert(0, "/tmp/shadow")\n\n\ndef test_x():\n    pass\n',
    }),
    Case("new_file_clears_an_imported_cache_at_import", True, {
        "test/tests/test_new.py": "from src import helpers\n\nhelpers.TIMEOUT.clear()\n\n\ndef test_x():\n    pass\n",
    }),
    Case("new_file_stores_into_an_imported_table_at_import", True, {
        # The store rule reaches import time too: `SEEN` is the module's own,
        # `helpers.TABLE` is not.
        "test/tests/test_new.py": 'from src import helpers\n\nSEEN = {}\nhelpers.TABLE["k"] = 1\n\n\ndef test_x():\n    pass\n',
    }),
    Case("plan_owned_structural_test_mutates_at_import", True, {
        "test/tests/test_smoke_coverage.py": 'import sys\n\nsys.path.append("/tmp/shadow")\n\n\ndef test_every_verb():\n    assert VERBS == set()\n',
    }),
    Case("new_file_class_body_mutates_an_imported_module", True, {
        # A class body runs at import as well, so the same rule holds in it.
        "test/tests/test_new.py": "from src import helpers\n\n\nclass TestX:\n    helpers.CACHE.clear()\n\n    def test_x(self):\n        pass\n",
    }),
    Case("new_file_module_level_constants_still_pass", False, {
        # What a real module does at import: constants, a path built from
        # `__file__`, a reference into an import, its own dict. None mutates
        # anything the existing tests resolve.
        "test/tests/test_new.py": "from pathlib import Path\n\nfrom src import helpers\n\n"
        + 'HERE = Path(__file__).resolve().parent\nROOT = HERE.parents[1]\nNAMES = frozenset({"a", "b"})\n'
        + "SEEN = {}\nSEEN['k'] = 1\nTABLE = helpers.TABLE\n\n\n"
        + "def test_x():\n    assert ROOT and NAMES and SEEN and TABLE is not None\n",
    }),
    # ---- L2 review: the config-key check is a permit list ------------------
    Case("config_adds_norecursedirs", True, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\nmarkers = []\nnorecursedirs = ['tests/oci']\n",
    }),
    Case("config_adds_collect_ignore", True, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\nmarkers = []\ncollect_ignore = ['tests/test_a.py']\n",
    }),
    Case("config_adds_collect_ignore_glob", True, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\nmarkers = []\ncollect_ignore_glob = ['*_oci.py']\n",
    }),
    Case("config_adds_an_unlisted_key", True, {
        # The point of the inversion: a key nobody enumerated still reds.
        "test/pyproject.toml": "[tool.pytest.ini_options]\nmarkers = []\nempty_parameter_set_mark = 'skip'\n",
    }),
    Case("config_adds_permitted_keys", False, {
        "test/pyproject.toml": "[tool.pytest.ini_options]\nmarkers = ['smoke']\naddopts = '--strict-markers'\n"
        + "pythonpath = ['.', 'src']\ntmp_path_retention_count = 1\ntmp_path_retention_policy = 'failed'\n",
    }),
    Case("taskfile_shell_assignment_is_not_a_config_key", False, {
        # The key check is `.toml`-only: a taskfile line is shell, where
        # `FLOOR=$(…)` is an assignment and not an ini setting.
        "test/taskfile.yml": "version: '3'\ntasks:\n  floor:\n    cmd: >-\n      FLOOR=$(cat SUITE_FLOOR);\n      echo \"$FLOOR\"\n",
        "test/SUITE_FLOOR": "3618\n",
    }),
    # ---- the cross-model (Codex) leads, each reproduced green before the fix ----
    Case("new_test_unpacks_a_shared_pair", True, {
        # Unpacking bound `b` to `helpers.PAIR`'s own element, not to a fresh
        # object — the store reaches the shared one.
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    a, b = helpers.PAIR\n    b["k"] = 1\n',
    }),
    Case("new_test_unpacks_a_literal_holding_a_shared_table", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    x, y = 1, helpers.TABLE\n    y["k"] = 1\n',
    }),
    Case("new_test_unpacks_a_literal_holding_a_module_global", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    a, b = 1, REGISTRY\n    b.clear()\n",
    }),
    Case("new_test_loops_over_a_literal_holding_a_shared_cache", True, {
        # Same laundering one statement further out: the loop item IS the
        # element the literal was handed.
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    for c in [helpers.CACHE]:\n        c.clear()\n",
    }),
    Case("new_test_mutates_a_list_it_built_around_a_parameter", False, {
        # The other side of that rule: a display literal bound to a NAME is a
        # fresh container, and mutating it reaches nothing the literal held —
        # the shape `test_smoke_coverage.py`'s own `todo = [fn]` worklist uses.
        "test/tests/test_a.py": _BASE_TEST
        + "\n\ndef test_x(ocx):\n    todo = [ocx]\n    todo.append(ocx)\n    seen = todo.pop()\n"
        + "    assert seen is ocx\n",
    }),
    Case("taskfile_adds_an_attached_k_expression", True, {
        # argparse takes a short option's value attached: `-kfoo` selects
        # exactly as `-k foo` does.
        "test/taskfile.yml": "version: '3'\ntasks:\n  quick:\n    cmd: uv run pytest -kfoo\n",
    }),
    Case("taskfile_adds_an_attached_m_expression", True, {
        "test/taskfile.yml": "version: '3'\ntasks:\n  quick:\n    cmd: uv run pytest -mnotsmoke\n",
    }),
    Case("taskfile_adds_an_attached_smoke_markexpr", False, {
        # The permitted spelling survives the widening: `-msmoke` is the
        # smoke tier's own marker, attached.
        "test/taskfile.yml": "version: '3'\ntasks:\n  smoke:\n    cmd: uv run pytest -msmoke -n auto\n",
    }),
    Case("docstring_line_gains_a_return", True, {
        # Proven under real pytest: with `; return` the module's assertion
        # never runs and the test passes, while the only changed line is the
        # docstring's — inert by the old rule.
        "test/tests/test_a.py": _edit(_BASE_TEST, '    """Runs about."""', '    """Runs about."""; return'),
    }),
    # ---- B1 review R7: the mutation rule is a permit list of pure reads ----
    # Four shapes the `_MUTATORS` enumeration this replaced let through, each
    # probed green against the pre-fix guard before the permit list landed.
    Case("new_file_writes_a_config_file_at_import", True, {
        # The file `_NEVER_NEW` refuses to let the diff ADD, written at
        # import instead: pytest prefers `test/pytest.ini` over
        # `pyproject.toml` and the whole `addopts` goes with it.
        "test/tests/test_new.py": 'from pathlib import Path\n\n'
        + 'Path("test/pytest.ini").write_text("[pytest]\\n")\n\n\ndef test_x():\n    pass\n',
    }),
    Case("new_test_sorts_an_imported_cache", True, {
        "test/tests/test_a.py": _BASE_TEST + "\n\ndef test_x(ocx):\n    helpers.CACHE.sort()\n",
    }),
    Case("new_test_calls_os_putenv", True, {
        # `os.environ.update` was enumerated; `os.putenv` is the same edit
        # one name over, and no enumeration of mutators ever catches it.
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    import os\n    os.putenv("OCX_HOME", "/tmp/shadow")\n',
    }),
    Case("new_file_removes_a_tree_under_an_import", True, {
        "test/tests/test_new.py": "import shutil\n\nfrom src import helpers\n\n\n"
        + "def test_x():\n    shutil.rmtree(helpers.ROOT)\n",
    }),
    Case("new_file_reads_through_modules_still_passes", False, {
        # The other polarity: the reads a real structural test makes — a
        # stdlib parse, a compiled pattern, a path read, and a call into the
        # suite's own frozen `test/src/**` API — all stay green.
        "test/tests/test_new.py": "import ast\nimport re\nfrom pathlib import Path\n\n"
        + "from src import helpers\n\n"
        + "HERE = Path(__file__).resolve().parent\nPATTERN = re.compile('x')\n\n\n"
        + "def test_x(ocx):\n"
        + "    tree = ast.parse(HERE.joinpath('a.py').read_text(encoding='utf-8'))\n"
        + "    names = [n for n in ast.walk(tree) if isinstance(n, ast.Name)]\n"
        + "    assert PATTERN.search('x') and helpers.make_package() == 1 and names is not None\n",
    }),
    # ---- B1 merge review: five verbs the permit list had wrong -------------
    # Each is a name whose *other* plausible receiver has a side effect that
    # outlives the test, so it left `_PURE_READS`; the green row below pins
    # the spelling that survives for each.
    Case("new_test_dumps_json_into_a_file_handle", True, {
        # `json.dump`/`yaml.dump` take a stream and write to it — the same
        # `write_text` escape with a second argument.
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    import json\n'
        + '    json.dump({"a": 1}, open("test/pytest.ini", "w", encoding="utf-8"))\n',
    }),
    Case("new_test_loads_from_a_file_handle", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    import json\n'
        + '    json.load(open("test/pyproject.toml", encoding="utf-8"))\n',
    }),
    Case("new_test_copies_a_file_over_an_import", True, {
        # `dict.copy()` reads; `shutil.copy(src, dst)` and `Path.copy` (3.14)
        # write a file.
        "test/tests/test_new.py": "import shutil\n\nfrom src import helpers\n\n\n"
        + 'def test_x():\n    shutil.copy(helpers.ROOT, "/tmp/shadow")\n',
    }),
    Case("new_test_fetches_over_the_network", True, {
        # `dict.get` reads; `requests.get` leaves the machine, against the
        # suite's offline posture.
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    import requests\n'
        + '    requests.get("http://example.invalid/x")\n',
    }),
    Case("new_test_imports_or_skips", True, {
        # `pytest.importorskip` imports: import-time side effects and a
        # `sys.modules` entry, the ground `.import_module(` is refused on.
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    pytest.importorskip("tomllib")\n',
    }),
    Case("new_test_unpickles_a_payload", True, {
        # `loads` stays a pure read for `json` and `tomllib`; the one member
        # of that family that runs arbitrary code is refused by module name.
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    import pickle\n'
        + '    pickle.loads(b"\\x80\\x04N.")\n',
    }),
    Case("new_file_serialises_without_touching_a_stream", False, {
        # What survives each removal: `read_text` + `loads` instead of
        # `load(fh)`, `dumps` instead of `dump(obj, fh)`, a subscript instead
        # of `.get(`, and `.copy()` on a dict this scope built.
        "test/tests/test_new.py": "import json\nfrom pathlib import Path\n\n"
        + "HERE = Path(__file__).resolve().parent\n\n\n"
        + "def test_x(ocx):\n"
        + "    data = json.loads(HERE.joinpath('a.json').read_text(encoding='utf-8'))\n"
        + "    seen = dict(data)\n"
        + "    assert json.dumps(data) and seen.copy() and data['k']\n",
    }),
    # ---- L2 review: a handle bound to a name is still a handle ------------
    # R7's own escape, one line longer: the one-liner
    # `Path("test/pytest.ini").write_text(…)` red while every bound form was
    # green, because a call result was read as this scope's own.
    Case("new_test_writes_through_a_bound_path_handle", True, {
        "test/tests/test_new.py": 'from pathlib import Path\n\n\ndef test_x():\n'
        + '    p = Path("test/pytest.ini")\n    p.write_text("[pytest]\\n")\n',
    }),
    Case("new_test_writes_through_a_bound_file_handle", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n'
        + '    f = open("test/pytest.ini", "w", encoding="utf-8")\n    f.write("x")\n',
    }),
    Case("new_test_writes_through_a_with_bound_handle", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n'
        + '    with open("test/pytest.ini", "w", encoding="utf-8") as f:\n        f.write("x")\n',
    }),
    Case("new_file_writes_through_a_module_level_handle", True, {
        # Opened at import, written inside the test. This one was already
        # red before the fix — a module global is foreign to a def's scope
        # whatever it holds — and is pinned here as the regression guard for
        # the case the three above escape through.
        "test/tests/test_new.py": 'from pathlib import Path\n\nINI = Path("test/pytest.ini")\n\n\n'
        + 'def test_x():\n    INI.write_text("[pytest]\\n")\n',
    }),
    Case("new_test_writes_into_its_own_tmp_path", False, {
        # The false-positive side of the same rule: a handle opened on what
        # the `tmp_path` fixture handed the test is the test's own, and
        # pytest takes it away again. Writing there must stay green.
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx, tmp_path):\n'
        + '    p = tmp_path.joinpath("x")\n    p.write_text("y")\n'
        + '    with open(p, "w", encoding="utf-8") as fh:\n        fh.write("z")\n'
        + '    assert p.read_text(encoding="utf-8") == "z"\n',
    }),
    # ---- L2 review: an import alias is not a way past _REFUSED_NAMES ------
    Case("new_test_unpickles_through_an_import_alias", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    import pickle as pk\n'
        + '    pk.loads(b"\\x80\\x04N.")\n',
    }),
    Case("new_test_unpickles_through_a_from_import", True, {
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    from marshal import loads as decode\n'
        + '    decode(b"\\x80\\x04N.")\n',
    }),
    # ---- L2 review: the deletion rule pairs lines, not token bags ---------
    Case("config_launders_a_dropped_option_via_another_key", True, {
        # `version` carrying the literal must not stand in for `addopts`.
        "test/pyproject.toml": '[project]\nversion = "--strict-markers"\n\n'
        + '[tool.pytest.ini_options]\nmarkers = []\naddopts = "-q"\n',
    }, base={"test/pyproject.toml": '[tool.pytest.ini_options]\nmarkers = []\naddopts = "--strict-markers -q"\n'}),
    Case("taskfile_launders_a_dropped_option_via_a_neighbour", True, {
        # Nor a line added beside the deletion inside the same hunk.
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmds:\n"
        + "      - echo --strict-markers\n      - uv run pytest -q\n",
    }, base={"test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest --strict-markers -q\n"}),
    Case("taskfile_restructures_keeping_every_option", False, {
        # The permitted edit: the invocation moves and keeps what it carried.
        "test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmds:\n"
        + "      - uv run pytest --strict-markers -q --color=no\n",
    }, base={"test/taskfile.yml": "version: '3'\ntasks:\n  parallel:\n    cmd: uv run pytest --strict-markers -q\n"}),
    # ---- L2 review: the mandated spelling for a copied environment --------
    Case("new_test_copies_the_environment_the_mandated_way", False, {
        # `os.environ.copy()` reds with `copy` off the permit list, and this
        # is the spelling that replaces it: `dict(…)` builds a value out of
        # its argument, so the result is this scope's own and freely mutable.
        "test/tests/test_a.py": _BASE_TEST + '\n\ndef test_x(ocx):\n    import os\n'
        + '    env = dict(os.environ)\n    env["OCX_OFFLINE"] = "1"\n'
        + '    assert env.get("OCX_OFFLINE") == "1" and env["OCX_OFFLINE"]\n',
    }),
    # ---- B1 review, cross-model (Codex): provenance is laundered two ways ----
    Case("new_test_launders_a_module_object_through_a_container", True, {
        # A fresh container makes the object it holds look owned: the list is
        # this scope's, `os.environ` inside it is not.
        "test/tests/test_a.py": _BASE_TEST
        + '\n\ndef test_x(ocx):\n    import os\n    box = [os.environ]\n    box[0]["OCX_OFFLINE"] = "1"\n',
    }),
    Case("new_test_aliases_a_mutator_and_calls_it_by_name", True, {
        # The mutation is the same one; only the syntax at the call site
        # changed, from an attribute call to a bare name.
        "test/tests/test_a.py": _BASE_TEST
        + '\n\ndef test_x(ocx):\n    import os\n    change = os.environ.update\n    change({"OCX_OFFLINE": "1"})\n',
    }),
    # ---- B1 review D19: the permit rule reaches the deletion direction ----
    Case("config_removes_strict_markers_from_addopts", True, {
        "test/pyproject.toml": '[tool.pytest.ini_options]\nmarkers = []\naddopts = "-q"\n',
    }, base={"test/pyproject.toml": '[tool.pytest.ini_options]\nmarkers = []\naddopts = "--strict-markers -q"\n'}),
    Case("config_respells_addopts_keeping_every_option", False, {
        # A re-spelling that keeps every option is the permitted edit.
        "test/pyproject.toml": '[tool.pytest.ini_options]\nmarkers = []\naddopts = "-q --strict-markers"\n',
    }, base={"test/pyproject.toml": '[tool.pytest.ini_options]\nmarkers = []\naddopts = "--strict-markers -q"\n'}),
    Case("taskfile_adds_the_smoke_tier", False, {
        # C-014's own tier, and prose/comment mentions of it, are not a narrowing.
        "test/taskfile.yml": "version: '3'\ntasks:\n  smoke:\n    desc: the smoke tier (`-m smoke`, 22 verbs)\n"
        + "    # never -k, never --deselect\n    cmd: uv run pytest -m smoke -n auto --dist loadgroup\n",
        "test/pyproject.toml": "[tool.pytest.ini_options]\n# run with `-m smoke -n auto` inside 90 s\nmarkers = ['smoke']\n",
    }),
    # ======================================================================
    # C-007 (plan_test_speed_tiers.md): the --tiered-shapes shapes (a)–(e) and
    # the floor rule. Every case here runs under the flag; each green gets a
    # `__flag_off` twin after the list (S-023). Names carry the shape letter.
    # ======================================================================
    # ---- (a) move to test/lint/ → green ------------------------------------
    Case("c007a_move_one_test_out_of_a_modified_module", False, {
        _STRUCTURE_PATH: _STRUCTURE_MINUS_ROWS_TEST,
        "test/lint/test_structure_rows.py": _LINT_ROWS,
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007a_move_whole_module", False, {
        # Every test def matched; the path constant `ROOT` is spelled
        # differently in the lint home, which a path constant may be.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007a_move_with_orphan_cleanup", False, {
        # `import tomllib`, `ROWS = …` and `def _rows` go with the moved test:
        # none of the three names occurs in the head module any more.
        _STRUCTURE_PATH: _STRUCTURE_MINUS_ORPHANS,
        "test/lint/test_structure_rows.py": _LINT_ROWS,
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    # ---- (a) move → red ----------------------------------------------------
    Case("c007a_body_changed_on_move_refused", True, {
        _STRUCTURE_PATH: _STRUCTURE_MINUS_ROWS_TEST,
        "test/lint/test_structure_rows.py": _edit(
            _LINT_ROWS, "assert isinstance(_rows(), dict)", "assert _rows() is not None"
        ),
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007a_deletion_without_counterpart_refused", True, {
        _STRUCTURE_PATH: _STRUCTURE_MINUS_ROWS_TEST,
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007a_whole_module_deleted_with_one_test_unmatched_refused", True, {
        # Whole-module sanction needs EVERY test def matched; only one is.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_rows.py": _LINT_ROWS,
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007a_orphan_removal_of_a_name_still_read_refused", True, {
        # `ROOT` is not orphaned: `ROWS` and `_read` still read it.
        _STRUCTURE_PATH: _edit(_STRUCTURE_MINUS_ROWS_TEST, "ROOT = Path(__file__).parent.parent\n", ""),
        "test/lint/test_structure_rows.py": _LINT_ROWS,
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007a_orphan_autouse_import_refused", True, {
        # Nothing the moved test reads: an import that acts by being there
        # (an autouse fixture) is not orphaned by the move.
        _AUTOUSE_PATH: "def test_keep():\n    assert 1\n",
        "test/lint/test_p3.py": "def test_moved():\n    assert 2\n",
    }, base={_AUTOUSE_PATH: _BASE_AUTOUSE}, tiered=True),
    Case("c007a_move_drops_class_decorator_refused", True, {
        # The method reads identical; the class it ran under lost its fixture.
        _CLASS_DECO_PATH: None,
        "test/lint/test_p5.py": "class TestX:\n    def test_m(self):\n        assert 1\n",
    }, base={_CLASS_DECO_PATH: _BASE_CLASS_DECO}, tiered=True),
    Case("c007a_move_drops_class_decorator_keeping_its_import_refused", True, {
        # As above with `pytest` still bound identically, so only `_move_key`'s
        # class context tells the two apart.
        _CLASS_DECO_PATH: None,
        "test/lint/test_p5.py": "import pytest\n\n\nclass TestX:\n    def test_m(self):\n        assert 1\n",
    }, base={_CLASS_DECO_PATH: _BASE_CLASS_DECO}, tiered=True),
    Case("c007a_one_lint_copy_sanctions_two_removals_refused", True, {
        # One added def sanctions one removal: the second deletion is unmatched.
        _DUP1_PATH: None,
        _DUP2_PATH: None,
        "test/lint/test_dup.py": _BASE_DUP,
    }, base={_DUP1_PATH: _BASE_DUP, _DUP2_PATH: _BASE_DUP}, tiered=True),
    Case("c007a_two_lint_copies_sanction_two_removals", False, {
        _DUP1_PATH: None,
        _DUP2_PATH: None,
        "test/lint/test_dup1.py": _BASE_DUP,
        "test/lint/test_dup2.py": _BASE_DUP,
    }, base={_DUP1_PATH: _BASE_DUP, _DUP2_PATH: _BASE_DUP}, tiered=True),
    Case("c007a_move_keeps_class_decorator", False, {
        _CLASS_DECO_PATH: None,
        "test/lint/test_p5.py": _BASE_CLASS_DECO,
    }, base={_CLASS_DECO_PATH: _BASE_CLASS_DECO}, tiered=True),
    Case("c007a_orphan_name_still_named_in_head_refused", True, {
        # `_rows` is read only by what left, but the head still names it (a
        # string or comment is how `usefixtures` and fixture requests read):
        # the occurrence rule alone keeps it.
        _STRUCTURE_PATH: _STRUCTURE_MINUS_ORPHANS + "\n# see _rows in test/lint/\n",
        "test/lint/test_structure_rows.py": _LINT_ROWS,
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    # ---- (a) move: the module-level names a moved test reads (L2 Block) ---
    Case("c007a_move_carries_constants_verbatim", False, {
        _CASES_PATH: None,
        "test/lint/test_cases.py": _BASE_CASES,
    }, base={_CASES_PATH: _BASE_CASES}, tiered=True),
    Case("c007a_move_empties_a_read_constant_refused", True, {
        # The adversary's shape: `test_check` reads identical, the constant
        # it loops over is emptied, and the failing assertion never runs.
        _CASES_PATH: None,
        "test/lint/test_cases.py": _edit(_BASE_CASES, "CASES = [0, False]", "CASES = []"),
    }, base={_CASES_PATH: _BASE_CASES}, tiered=True),
    Case("c007a_move_mutates_a_read_constant_refused", True, {
        # The binding is verbatim; a later top-level call empties it.
        _CASES_PATH: None,
        "test/lint/test_cases.py": _BASE_CASES + "\n\nCASES.clear()\n",
    }, base={_CASES_PATH: _BASE_CASES}, tiered=True),
    Case("c007a_move_changes_a_helper_refused", True, {
        # Reached through the helper `_each`, not the test body.
        _CASES_PATH: None,
        "test/lint/test_cases.py": _edit(_BASE_CASES, "return list(CASES)", "return []"),
    }, base={_CASES_PATH: _BASE_CASES}, tiered=True),
    Case("c007a_move_rewrites_a_path_helper_refused", True, {
        # `_rows` is a helper, not a path constant: `return {}` passes the
        # `isinstance(_rows(), dict)` assertion whatever the file holds.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _edit(
            _LINT_STRUCTURE, 'return tomllib.loads(ROWS.read_text(encoding="utf-8"))', "return {}"
        ),
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007a_move_allow_listed_rewrite", False, {
        # A reviewed rewrite: the lint home's `CASES` line is named by
        # `--allow`, so the difference is the reviewer's, not silent.
        _CASES_PATH: None,
        "test/lint/test_cases.py": _edit(_BASE_CASES, "CASES = [0, False]", "CASES = [0, False, None]"),
    }, base={_CASES_PATH: _BASE_CASES}, allow=["test/lint/test_cases.py:1"], tiered=True),
    Case("c007a_move_allow_listed_rewrite_reading_a_new_name_refused", True, {
        # The allowed rewrite reads `MORE`, unbound in the source: held to
        # the same rule, and not itself allow-listed.
        _CASES_PATH: None,
        "test/lint/test_cases.py": _edit(_BASE_CASES, "CASES = [0, False]", "MORE = []\nCASES = MORE"),
    }, base={_CASES_PATH: _BASE_CASES}, allow=["test/lint/test_cases.py:2"], tiered=True),
    Case("c007a_move_shadows_a_builtin_refused", True, {
        # Unbound in the source (the builtin), bound in the lint home.
        _CASES_PATH: None,
        "test/lint/test_cases.py": "def list(_):\n    return [1]\n\n\n" + _BASE_CASES,
    }, base={_CASES_PATH: _BASE_CASES}, tiered=True),
    # ---- (b) new lint files → green / red ----------------------------------
    Case("c007b_lint_conftest_test_file_and_lint_floor", False, {
        "test/lint/conftest.py": 'pytest_plugins = ["pytester"]\n',
        "test/lint/test_lint_tier_guards.py": "def test_lint_tier_collects():\n    assert True\n",
        "test/LINT_FLOOR": "1\n",
    }, tiered=True),
    Case("c007b_lint_init_py_refused", True, {
        "test/lint/conftest.py": 'pytest_plugins = ["pytester"]\n',
        "test/lint/__init__.py": "",
    }, tiered=True),
    Case("c007b_lint_data_file_refused", True, {
        "test/lint/expected_rows.json": "{}\n",
    }, tiered=True),
    Case("c007b_lint_subdirectory_refused", True, {
        # Direct children only: a nested test module is "anything else".
        "test/lint/nested/test_deep.py": "def test_deep():\n    assert True\n",
    }, tiered=True),
    # ---- (b) lint file CONTENT → green / red (L1 review Block) -------------
    Case("c007b_lint_conftest_c008_shape", False, {
        "test/lint/conftest.py": _LINT_CONFTEST_C008,
    }, tiered=True),
    Case("c007b_lint_module_carries_verbatim_source_context", False, {
        # The whole module moves verbatim: its skipif and its subprocess call
        # are code already in the suite, judged there, not new lint code.
        _SWEEP_PATH: None,
        "test/lint/test_sweep.py": _BASE_SWEEP,
    }, base={_SWEEP_PATH: _BASE_SWEEP}, tiered=True),
    Case("c007b_lint_module_carries_verbatim_helper", False, {
        # A helper moved unchanged is code the suite already ran, like a
        # verbatim module-level statement — its `subprocess.run` is not new.
        _SWEEP_HELPER_PATH: None,
        "test/lint/test_sweep_helper.py": _BASE_SWEEP_HELPER,
    }, base={_SWEEP_HELPER_PATH: _BASE_SWEEP_HELPER}, tiered=True),
    Case("c007b_lint_module_helper_changed_on_move_refused", True, {
        # One argument added on the way: no longer the suite's code, so the
        # DEC-10(b) walk judges it and refuses the spawn.
        _SWEEP_HELPER_PATH: None,
        "test/lint/test_sweep_helper.py": _edit(_BASE_SWEEP_HELPER, '"ls-files"]', '"ls-files", "-z"]'),
    }, base={_SWEEP_HELPER_PATH: _BASE_SWEEP_HELPER}, tiered=True),
    Case("c007b_lint_module_skip_mark_refused", True, {
        # A moved test landing in a module that skips it never runs.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": 'import pytest\n\npytestmark = pytest.mark.skip(reason="x")\n'
        + _LINT_STRUCTURE,
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_module_bare_skip_mark_refused", True, {
        # No call, so the DEC-10(b) walk has nothing to refuse: the skip scan
        # alone stops it.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": "import pytest\n\npytestmark = pytest.mark.skip\n" + _LINT_STRUCTURE,
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_module_new_code_rebinds_subprocess_refused", True, {
        # New module-level code gets DEC-10(b)'s walk: neutering subprocess
        # makes every moved `git ls-files` sweep vacuous.
        _SWEEP_PATH: None,
        "test/lint/test_sweep.py": _BASE_SWEEP + "\nsubprocess.run = print\n",
    }, base={_SWEEP_PATH: _BASE_SWEEP}, tiered=True),
    Case("c007b_lint_conftest_collect_ignore_refused", True, {
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": 'collect_ignore_glob = ["test_*.py"]\n',
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_modifyitems_skip_refused", True, {
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": "def pytest_collection_modifyitems(items):\n    items.clear()\n",
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_skip_in_setup_refused", True, {
        "test/lint/conftest.py": "import pytest\n\n\ndef pytest_runtest_setup(item):\n    pytest.skip(\"x\")\n",
    }, tiered=True),
    Case("c007b_lint_conftest_c008_budget_as_exit_code", False, {
        # The budget hook may fail the session by pytest.ExitCode too.
        "test/lint/conftest.py": _LINT_CONFTEST_C008.replace(
            "session.exitstatus = 1", "session.exitstatus = pytest.ExitCode.TESTS_FAILED"
        ),
    }, tiered=True),
    Case("c007b_lint_conftest_sessionfinish_passes_session_refused", True, {
        # L1 re-check probe N1: flips a verdict rather than a collection.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": 'def pytest_sessionfinish(session, exitstatus):\n    session.exitstatus = 0\n',
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_sessionfinish_exit_code_ok_refused", True, {
        # L1 re-check probe N1: flips a verdict rather than a collection.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": 'import pytest\n\n\ndef pytest_sessionfinish(session, exitstatus):\n    session.exitstatus = pytest.ExitCode.OK\n',
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_logreport_flips_outcome_refused", True, {
        # L1 re-check probe N2: flips a verdict rather than a collection.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": "def pytest_runtest_logreport(report):\n    report.outcome = 'passed'\n",
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_unittest_skiptest_refused", True, {
        # L1 re-check probe N3: flips a verdict rather than a collection.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": "import unittest\n\n\ndef pytest_runtest_setup(item):\n    raise unittest.SkipTest('x')\n",
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_private_skipped_refused", True, {
        # L1 re-check probe N4: flips a verdict rather than a collection.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": "from _pytest.outcomes import Skipped\n\n\ndef pytest_runtest_setup(item):\n    raise Skipped('x')\n",
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_hookimpl_wrapper_refused", True, {
        # L1 re-check probe N5: flips a verdict rather than a collection.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": 'import pytest\n\n\n@pytest.hookimpl(wrapper=True)\ndef pytest_runtest_teardown(item):\n    yield\n',
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_hookimpl_hookwrapper_refused", True, {
        # L1 re-check probe N5: flips a verdict rather than a collection.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": 'import pytest\n\n\n@pytest.hookimpl(hookwrapper=True)\ndef pytest_runtest_teardown(item):\n    yield\n',
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_setattr_outcome_by_name_refused", True, {
        # L1 re-check probe N2: flips a verdict rather than a collection.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": "def pytest_runtest_setup(item):\n    item.stash.setattr(item, 'outcome', 'passed')\n",
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_conftest_exits_early_green_refused", True, {
        # L1 re-check probe N1: flips a verdict rather than a collection.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/lint/conftest.py": 'import os\n\n\ndef pytest_sessionfinish(session, exitstatus):\n    os._exit(0)\n',
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE}, tiered=True),
    Case("c007b_lint_module_foreign_skipif_carried_refused", True, {
        # L1 re-check probe N6: two modules move in one range; A's lint home
        # copies B's darwin-only skipif, verbatim from a source module — but
        # not from one whose tests this file carries.
        _AA_PATH: None,
        _BB_PATH: None,
        "test/lint/test_aa.py": _LINT_AA_WITH_BB_SKIPIF,
        "test/lint/test_bb.py": _BASE_BB,
    }, base={_AA_PATH: _BASE_AA, _BB_PATH: _BASE_BB}, tiered=True),
    Case("c007b_lint_conftest_foreign_plugin_refused", True, {
        "test/lint/conftest.py": 'pytest_plugins = ["pytester", "src.helpers"]\n',
    }, tiered=True),
    # ---- (b) lint conftest writing execution state (L2 Block) ---------------
    # An admission hook is open, but the objects it is handed are read-only:
    # `item.obj = lambda: None` turned a failing test into a passing one with
    # every count exact. The live conftest's green is the `live_lint_conftest`
    # probe.
    Case("c007b_lint_conftest_replaces_the_test_callable_refused", True, {
        "test/lint/conftest.py": "def pytest_runtest_setup(item):\n    item.obj = lambda: None\n",
    }, tiered=True),
    Case("c007b_lint_conftest_patches_the_item_class_refused", True, {
        "test/lint/conftest.py": "def pytest_runtest_setup(item):\n    type(item).runtest = lambda self: None\n",
    }, tiered=True),
    Case("c007b_lint_conftest_mutates_through_a_helper_refused", True, {
        "test/lint/conftest.py": "def _n(x):\n    x.obj = None\n\n\ndef pytest_runtest_setup(item):\n    _n(item)\n",
    }, tiered=True),
    Case("c007b_lint_conftest_adds_a_marker_refused", True, {
        # `pytest_itemcollected` is no named collection hook; an xfail mark
        # added there is evaluated at setup and passes a failing test.
        "test/lint/conftest.py": 'def pytest_itemcollected(item):\n    item.add_marker("xfail")\n',
    }, tiered=True),
    Case("c007b_lint_conftest_alias_through_a_loop_refused", True, {
        "test/lint/conftest.py": (
            "def pytest_collection_finish(session):\n    for it in sorted(session.items):\n        it.obj = None\n"
        ),
    }, tiered=True),
    Case("c007b_lint_conftest_monkeypatches_the_node_refused", True, {
        "test/lint/conftest.py": (
            "import pytest\n\n\n@pytest.fixture(autouse=True)\ndef f(request, monkeypatch):\n"
            "    monkeypatch.setattr(request.node, 'obj', None)\n"
        ),
    }, tiered=True),
    Case("c007b_lint_conftest_vars_write_refused", True, {
        "test/lint/conftest.py": "def pytest_runtest_setup(item):\n    vars(item)['obj'] = None\n",
    }, tiered=True),
    Case("c007b_lint_conftest_registers_a_plugin_refused", True, {
        "test/lint/conftest.py": (
            "class P:\n    def pytest_itemcollected(self, item):\n        pass\n\n\n"
            "def pytest_configure(config):\n    config.pluginmanager.register(P())\n"
        ),
    }, tiered=True),
    Case("c007b_lint_conftest_hookimpl_specname_refused", True, {
        "test/lint/conftest.py": (
            "import pytest\n\n\n@pytest.hookimpl(specname='pytest_pyfunc_call')\ndef fine(pyfuncitem):\n"
            "    return True\n"
        ),
    }, tiered=True),
    # ---- (c) `command` marker line → green (one per binding form) ----------
    Case("c007c_marker_where_no_pytestmark_is_bound", False, {
        "test/tests/test_a.py": _edit(
            _BASE_TEST, "# a comment line\n",
            'pytestmark = pytest.mark.command("install", "package pull")\n# a comment line\n',
        ),
    }, tiered=True),
    Case("c007c_marker_wraps_a_single_skipif_mark", False, {
        # The form test_shell_activation.py:88 uses; the skipif line itself
        # is untouched, so the hunk removes nothing.
        _LOGIN_PATH: _edit(
            _BASE_LOGIN, f"pytestmark = {_SKIPIF}\n",
            f'pytestmark = {_SKIPIF}\npytestmark = [pytestmark, pytest.mark.command("login")]\n',
        ),
    }, base={_LOGIN_PATH: _BASE_LOGIN}, tiered=True),
    Case("c007c_marker_appended_to_a_mark_list", False, {
        # The form test_shell_reconcile.py:64 uses.
        _RECONCILE_PATH: _edit(
            _BASE_RECONCILE, f"pytestmark = [{_SKIPIF}]\n",
            f'pytestmark = [{_SKIPIF}]\npytestmark.append(pytest.mark.command("shell hook"))\n',
        ),
    }, base={_RECONCILE_PATH: _BASE_RECONCILE}, tiered=True),
    # ---- (c) marker → red ---------------------------------------------------
    Case("c007c_marker_non_literal_argument_refused", True, {
        "test/tests/test_a.py": _edit(
            _BASE_TEST, "--COUNT\n", "--COUNT\npytestmark = pytest.mark.command(SCRIPT)\n"
        ),
    }, tiered=True),
    Case("c007c_marker_keyword_argument_refused", True, {
        "test/tests/test_a.py": _edit(
            _BASE_TEST, "# a comment line\n",
            'pytestmark = pytest.mark.command("install", reason="x")\n# a comment line\n',
        ),
    }, tiered=True),
    Case("c007c_marker_combined_with_skip_refused", True, {
        _LOGIN_PATH: _edit(
            _BASE_LOGIN, f"pytestmark = {_SKIPIF}\n",
            f"pytestmark = {_SKIPIF}\n"
            'pytestmark = [pytestmark, pytest.mark.skipif(sys.platform == "darwin", reason="y"), '
            'pytest.mark.command("x")]\n',
        ),
    }, base={_LOGIN_PATH: _BASE_LOGIN}, tiered=True),
    Case("c007c_existing_skipif_rewritten_into_a_list_refused", True, {
        # Same resulting marks as the green wrap, but the skipif line is a
        # removed line in the marker's hunk.
        _LOGIN_PATH: _edit(
            _BASE_LOGIN, f"pytestmark = {_SKIPIF}\n",
            f'pytestmark = [{_SKIPIF}, pytest.mark.command("login")]\n',
        ),
    }, base={_LOGIN_PATH: _BASE_LOGIN}, tiered=True),
    Case("c007c_append_after_a_single_mark_refused", True, {
        # The list form, where the module binds a single mark: wrong form.
        _LOGIN_PATH: _edit(
            _BASE_LOGIN, f"pytestmark = {_SKIPIF}\n",
            f'pytestmark = {_SKIPIF}\npytestmark.append(pytest.mark.command("login"))\n',
        ),
    }, base={_LOGIN_PATH: _BASE_LOGIN}, tiered=True),
    Case("c007c_bare_assignment_over_a_single_mark_refused", True, {
        # The no-binding form where one exists: it silently drops the skipif.
        _LOGIN_PATH: _edit(
            _BASE_LOGIN, f"pytestmark = {_SKIPIF}\n",
            f'pytestmark = {_SKIPIF}\npytestmark = pytest.mark.command("login")\n',
        ),
    }, base={_LOGIN_PATH: _BASE_LOGIN}, tiered=True),
    Case("c007c_new_test_decorated_with_command_is_a_new_test", False, {
        # Not a marker statement: a new def is judged by the new-test rules,
        # which admit any `@pytest.mark.*` — the same verdict as flag-off.
        "test/tests/test_a.py": _BASE_TEST
        + '\n\n@pytest.mark.command("install")\ndef test_install(ocx):\n    assert ocx.run("install").returncode == 0\n',
    }, tiered=True, twin=False),
    Case("c007c_marker_hunk_removes_a_comment_refused", True, {
        # Only the "no removed line in its hunk" clause stops this one: the
        # replaced line is a comment, which the line checks let go.
        "test/tests/test_a.py": _edit(
            _BASE_TEST, "# a comment line\n", 'pytestmark = pytest.mark.command("install")\n',
        ),
    }, tiered=True),
    Case("c007c_marker_below_earlier_removals", False, {
        # WP-06's false red: comment removals above the marker shift head
        # numbering, so a removal hunk's OLD lines (2-4, 6) overlap the
        # marker's NEW lines (4-6). Neither is the marker's hunk, which
        # removes nothing — judged by `covers` (either side) it read as one.
        _SHIFT_PATH: (
            "import pytest\nA = 1\nB = 2\n"
            'pytestmark = pytest.mark.command(\n    "install",\n)\n'
            "\n\ndef test_x():\n    assert A + B == 3\n"
        ),
    }, base={_SHIFT_PATH: (
        "import pytest\n# c1\n# c2\n# c3\nA = 1\n# doomed\nB = 2\n"
        "\n\ndef test_x():\n    assert A + B == 3\n"
    )}, tiered=True),
    Case("c007c_marker_before_the_last_pytestmark_binding_refused", True, {
        _LOGIN_PATH: _edit(
            _BASE_LOGIN, f"pytestmark = {_SKIPIF}\n",
            f'pytestmark = [pytestmark, pytest.mark.command("login")]\npytestmark = {_SKIPIF}\n',
        ),
    }, base={_LOGIN_PATH: _BASE_LOGIN}, tiered=True),
    # ---- (d) config → green / red ------------------------------------------
    Case("c007d_new_scoped_rows_key", False, {
        # A rows key is not an ini key: the permit-list is test/pyproject.toml's.
        "test/scoped_rows.toml": 'ocx_setup = ["tests/test_self_setup.py"]\n',
    }, tiered=True),
    Case("c007d_unlisted_pyproject_key_refused", True, {
        # The key rule still holds for test/pyproject.toml under the flag.
        "test/pyproject.toml": '[tool.pytest.ini_options]\nmarkers = []\nconsole_output_style = "classic"\n',
    }, tiered=True),
    Case("c007d_scoped_rows_selection_token_refused", True, {
        # The selection-token check still reads every rows line.
        "test/scoped_rows.toml": 'ocx_setup = ["--deselect", "tests/test_self_setup.py::test_x"]\n',
    }, tiered=True),
    Case("c007d_cargo_release_respelled_as_profile", False, {
        # `--release` is cargo's `--profile release`; the value is not compared,
        # so this is the long spelling's `--profile release` -> `test-bin`.
        "test/taskfile.yml": _edit(_CARGO_TASKFILE, "--release", "--profile test-bin"),
    }, base={"test/taskfile.yml": _CARGO_TASKFILE}, tiered=True),
    Case("c007d_cargo_release_respelled_dropping_locked_refused", True, {
        # The profile may change; every other option must still survive.
        "test/taskfile.yml": _edit(_CARGO_TASKFILE, "--release -p ocx --locked", "--profile test-bin -p ocx"),
    }, base={"test/taskfile.yml": _CARGO_TASKFILE}, tiered=True),
    Case("c007d_release_off_a_cargo_line_refused", True, {
        # Not cargo: `--release` is nothing's abbreviation here.
        "test/taskfile.yml": _edit(
            _CARGO_TASKFILE.replace("cargo build", "./ship.sh"), "--release", "--profile test-bin"
        ),
    }, base={"test/taskfile.yml": _CARGO_TASKFILE.replace("cargo build", "./ship.sh")}, tiered=True),
    # ---- (e) ported-from deletion → green ----------------------------------
    Case("c007e_ported_deletion_at_tip", False, dict(_PORT_HEAD), tiered=True, junit=_JUNIT_PORT_PASSED),
    Case("c007e_ported_deletion_post_merge", False, dict(_PORT_HEAD),
         tiered=True, junit=_JUNIT_PORT_PASSED, head_state="merge"),
    Case("c007e_ported_class_method_deletion", False, {
        # A method port: the marker spells the case pytest's way
        # (`Class::method`), and the emptied class goes with its method.
        _CLS_PATH: _CLS_MINUS_CLASS,
        "crates/ocx_lib/src/lib.rs": _PORTED_METHOD_LIB_RS,
    }, base={_CLS_PATH: _BASE_CLS}, tiered=True, junit=_JUNIT_TEMPLATE.format(name="tests::old_method_is_true", tail="/>")),
    # ---- (e) → red (S-020) -------------------------------------------------
    Case("c007e_port_absent_from_report_refused", True, dict(_PORT_HEAD),
         tiered=True, junit=_JUNIT_PORT_ABSENT),
    Case("c007e_ignored_port_refused", True, {
        "test/tests/test_old.py": None,
        "crates/ocx_lib/src/lib.rs": _PORTED_LIB_RS_IGNORED,
    }, tiered=True, junit=_JUNIT_PORT_IGNORED),
    Case("c007e_failed_port_testcase_refused", True, dict(_PORT_HEAD), tiered=True,
         junit=_JUNIT_TEMPLATE.format(name="tests::old_is_true", tail='><failure message="x"/></testcase>')),
    Case("c007e_stale_report_refused", True, dict(_PORT_HEAD),
         tiered=True, junit=None, stale_junit=_JUNIT_PORT_PASSED),
    Case("c007e_failed_unit_run_refused", True, dict(_PORT_HEAD),
         tiered=True, junit=_JUNIT_PORT_PASSED, unit_exit=1),
    Case("c007e_tip_not_ancestor_of_head_refused", True, dict(_PORT_HEAD),
         tiered=True, junit=_JUNIT_PORT_PASSED, head_state="behind"),
    Case("c007e_dirty_tree_refused", True, dict(_PORT_HEAD),
         tiered=True, junit=_JUNIT_PORT_PASSED, head_state="dirty"),
    # ---- floor: SUITE_FLOOR decrease D vs sanctioned collected count N -----
    Case("c007_floor_decrease_equals_sanctioned_count", False, {
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/SUITE_FLOOR": "8\n",  # D=2, N=2
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE, "test/SUITE_FLOOR": "10\n"}, tiered=True),
    Case("c007_floor_decrease_below_sanctioned_count", False, {
        # D=1 < N=2. Pins the orchestrator's `D <= N` reading of C-007's
        # "must equal": a floor lowered by less than it could be stays green.
        _STRUCTURE_PATH: None,
        "test/lint/test_structure_lint.py": _LINT_STRUCTURE,
        "test/SUITE_FLOOR": "9\n",
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE, "test/SUITE_FLOOR": "10\n"}, tiered=True),
    Case("c007_lint_floor_raised", False, {
        "test/LINT_FLOOR": "441\n",
        "test/LINT_SKIP_CEILING": "3\n",
        "test/LINT_XFAIL_CEILING": "1\n",
    }, base={"test/LINT_FLOOR": "440\n", "test/LINT_SKIP_CEILING": "2\n", "test/LINT_XFAIL_CEILING": "0\n"}, tiered=True),
    Case("c007_lint_floor_lowered_refused", True, {
        "test/LINT_FLOOR": "1\n",
    }, base={"test/LINT_FLOOR": "440\n"}, tiered=True),
    Case("c007_plan_owned_lint_home_edit", False, {
        # PLAN_OWNED_STRUCTURAL_TESTS at its test/lint/ home: an oracle the
        # plan revises gets the whole-module walk, not the assertion freeze.
        "test/lint/test_logging.py": "A = 2\n\n\ndef test_rows():\n    assert A == 2\n",
    }, base={"test/lint/test_logging.py": "A = 1\n\n\ndef test_rows():\n    assert A == 1\n"}),
    Case("c007_floor_decrease_exceeds_sanctioned_count_refused", True, {
        _STRUCTURE_PATH: _STRUCTURE_MINUS_ROWS_TEST,
        "test/lint/test_structure_rows.py": _LINT_ROWS,
        "test/SUITE_FLOOR": "8\n",  # D=2, N=1
    }, base={_STRUCTURE_PATH: _BASE_STRUCTURE, "test/SUITE_FLOOR": "10\n"}, tiered=True),
]

# S-023: every tiered green, checked WITHOUT the flag, is refused exactly as
# today — the shapes are opt-in (DEC-10 default unchanged).
SELF_TEST_CASES += [
    replace(case, name=f"{case.name}__flag_off", tiered=False, expect_red=True)
    for case in SELF_TEST_CASES
    if case.tiered and not case.expect_red and case.twin
]


def _write_tree(repo: Path, files: dict[str, str | None]) -> None:
    for rel, content in files.items():
        path = repo / rel
        if content is None:
            path.unlink()
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")


def _run_case(case: Case, scratch: Path) -> tuple[bool, list[str]]:
    """Build the case's repository, run the guard on `base..head`; (red?, problems)."""
    repo = scratch / case.name
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "user.email", "guard@example.invalid")
    git(repo, "config", "user.name", "guard")
    git(repo, "config", "commit.gpgsign", "false")
    _write_tree(repo, _BASE_TREE | case.base)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "base")
    base = git(repo, "rev-parse", "HEAD").strip()
    if case.head_state == "merge":
        git(repo, "checkout", "-q", "-b", "wp")
    _write_tree(repo, case.head)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "head")
    head = git(repo, "rev-parse", "HEAD").strip()
    # The mutation must have landed: a case whose head equals its base would
    # be green for the wrong reason.
    if not git(repo, "diff", "--name-only", base, head).split():
        raise SystemExit(f"self-test: {case.name}: head commit changed nothing — a green here would be vacuous")
    if case.head_state == "merge":
        git(repo, "checkout", "-q", "main")
        git(repo, "merge", "-q", "--no-ff", "-m", "merge wp", "wp")
        if git(repo, "rev-parse", "HEAD^2").strip() != head:
            raise SystemExit(f"self-test: {case.name}: HEAD is not a merge commit whose second parent is the tip")
    elif case.head_state == "behind":
        git(repo, "checkout", "-q", "--detach", base)
    elif case.head_state == "dirty":
        (repo / "stray.txt").write_text("untracked\n", encoding="utf-8")
    elif case.head_state != "tip":
        raise SystemExit(f"self-test fixture: {case.name}: unknown head_state {case.head_state!r}")
    tiered = None
    if case.tiered:
        report = repo / UNIT_JUNIT
        if case.stale_junit is not None:
            report.parent.mkdir(parents=True, exist_ok=True)
            report.write_text(case.stale_junit, encoding="utf-8")

        def runner(where: Path) -> int:
            if case.junit is not None:
                report.parent.mkdir(parents=True, exist_ok=True)
                report.write_text(case.junit, encoding="utf-8")
            return case.unit_exit

        tiered = Tiered(unit_runner=runner, collector=lambda _repo, _base, ids: len(ids))
    problems = check_range(repo, base, head, parse_allow(case.allow), tiered)
    return bool(problems), problems


def _probe_repo(scratch: Path, name: str, files: dict[str, str]) -> tuple[Path, str]:
    """A throwaway repository holding exactly `files`; `(repo, head)`."""
    repo = scratch / name
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "user.email", "guard@example.invalid")
    git(repo, "config", "user.name", "guard")
    git(repo, "config", "commit.gpgsign", "false")
    _write_tree(repo, files)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "base")
    return repo, git(repo, "rev-parse", "HEAD").strip()


def _floor_probes(scratch: Path) -> list[tuple[str, bool, str]]:
    """`_invoked_by_a_taskfile` shown refusing to judge, and shown judging.

    The refusals are the point: a reader that quietly matched nothing would
    answer `False` for every path, which reads as "no task runs this" and is
    the over-strict classification this route was added to correct — arriving
    with no message to say the scan broke. Both floors must therefore answer
    `None`, and the control beneath them is what makes that non-vacuous: a
    reader stuck on `None` would satisfy the refusals and prove nothing.

    Direct rather than through `Case`: a floor is a property of a *tree* that
    lacks something, and the case harness only ever adds to `_BASE_TREE`.
    """
    script = "test/scripts/sync_thing.sh"
    asset = "test/fixtures/pinned/inert_asset.json"
    contents = {script: "#!/bin/sh\n", asset: "{}\n"}
    invoking = "version: '3'\ntasks:\n  drift:\n    cmd: ./test/scripts/sync_thing.sh --check\n"
    probes: list[tuple[str, bool, str]] = []

    repo, head = _probe_repo(scratch, "floor-no-taskfile", contents | {"taskfile.yml": invoking})
    answer = _invoked_by_a_taskfile(repo, head, script)
    probes.append((
        "no test/taskfile.yml in the tree refuses to judge",
        answer is None,
        f"answered {answer!r}, not None — a tree with no acceptance suite was judged anyway",
    ))

    repo, head = _probe_repo(scratch, "floor-no-invocation", contents | {
        "test/taskfile.yml": "version: '3'\n",
        "taskfile.yml": "version: '3'\ntasks:\n  build:\n    cmd: cargo build\n",
    })
    answer = _invoked_by_a_taskfile(repo, head, script)
    probes.append((
        "taskfiles that invoke nothing under test/ refuse to judge",
        answer is None,
        (
            f"answered {answer!r}, not None — 'no task runs it' would be indistinguishable "
            "from a scan that read nothing"
        ),
    ))

    repo, head = _probe_repo(scratch, "floor-control", contents | {
        "test/taskfile.yml": "version: '3'\n",
        "taskfile.yml": invoking,
    })
    invoked = _invoked_by_a_taskfile(repo, head, script)
    inert = _invoked_by_a_taskfile(repo, head, asset)
    probes.append((
        "the control reads True for the invoked script and False for the asset beside it",
        (invoked, inert) == (True, False),
        (
            f"answered {(invoked, inert)!r}, not (True, False) — the refusals above would "
            "hold for a reader stuck on one answer"
        ),
    ))
    return probes


def _collect_probes(scratch: Path) -> list[tuple[str, bool, str]]:
    """`collect_count` itself — every `Case` fakes it — at a base whose test
    module puts `website/scripts` on `sys.path` and imports from it, as
    `test_doc_scripts_publish.py` does. A `test/`-only export fails that import."""
    repo, base = _probe_repo(scratch, "collect-website-import", {
        "website/scripts/probe_render.py": "VALUE = 1\n",
        "test/tests/test_probe.py": (
            "import sys\nfrom pathlib import Path\n\n"
            'sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "website" / "scripts"))\n'
            "from probe_render import VALUE  # noqa: E402\n\n\n"
            "def test_probe():\n    assert VALUE == 1\n"
        ),
    })
    try:
        count: int | str = collect_count(repo, base, ["tests/test_probe.py::test_probe"])
    except SystemExit as refused:
        count = str(refused).splitlines()[0]
    return [(
        "collect_count collects a base module importing website/scripts",
        count == 1,
        f"got {count!r}, not 1",
    )]


def _live_lint_conftest_probe() -> tuple[str, bool, str]:
    """The C-008 conftest as it is in the tree must stay admitted (b): the green twin of every hook-state red."""
    problems = _lint_conftest_problems((REPO_ROOT / "test" / "lint" / "conftest.py").read_text(encoding="utf-8"))
    return "live_lint_conftest", not problems, "; ".join(problems)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def parse_allow(specs: list[str]) -> frozenset[tuple[str, int]]:
    """`PATH:LINE` specs (argparse already stripped the `--allow`) → pairs."""
    allow: set[tuple[str, int]] = set()
    for spec in specs:
        path, sep, line = spec.rpartition(":")
        if not sep or not line.isdigit():
            raise SystemExit(f"--allow expects <path>:<line>, got {spec!r}")
        allow.add((path, int(line)))
    return frozenset(allow)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("range", nargs="?", help="<base>..<head> (or <base>...<head>)")
    parser.add_argument(
        "--allow", action="append", default=[], metavar="PATH:LINE",
        help="allow-list the one hunk touching PATH at LINE (either side of the diff)",
    )
    parser.add_argument(
        "--tiered-shapes", action="store_true",
        help="also admit plan_test_speed_tiers C-007's shapes: moves to test/lint/, lint files, "
        "command marker lines, scoped_rows.toml/LINT_FLOOR, ported-from deletions",
    )
    ns = parser.parse_args(argv)
    if not ns.range:
        parser.error("a <base>..<head> range is required")
    base, head = split_range(ns.range)
    if "..." in ns.range:
        base = git(REPO_ROOT, "merge-base", base, head).strip()
    problems = check_range(REPO_ROOT, base, head, parse_allow(ns.allow), Tiered() if ns.tiered_shapes else None)
    for p in problems:
        print(p)
    if problems:
        print(f"test-diff guard: {len(problems)} violation(s) in {ns.range} under test/", file=sys.stderr)
        return 1
    print(f"test-diff guard: {ns.range} changes nothing the acceptance suite asserts")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
