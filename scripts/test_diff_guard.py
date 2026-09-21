#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Refuse a diff that changes what the acceptance suite asserts.

    scripts/test_diff_guard.py <base>..<head> [--allow test/tests/<file>.py:<line>]...
    scripts/test_diff_guard.py --self-test

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
in one commit.

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

Stdlib only; driven by `git diff`. `--self-test` builds one throwaway git
repository per shape under `<repo>/.tmp/` and shows the guard red on every
forbidden shape and green on every allowed one — a guard that was never seen
red is a habit, not a check.
"""
from __future__ import annotations

import argparse
import ast
import io
import re
import sys
import tempfile
import tokenize
from dataclasses import dataclass, field
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
    if path.endswith(".toml") and (m := _CONFIG_KEY.match(text)) and m["key"] not in _PERMITTED_CONFIG_KEYS:
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


def _new_file_allowed(path: str) -> bool:
    """Only a new `test/tests/**/*.py` module — the (b) of DEC-10 — may appear.

    Everything else is refused by shape rather than by an enumeration of
    what is dangerous: a `test/pytest.ini` beats `pyproject.toml` in pytest's
    config search and can drop `--strict-markers` or `--deselect` a test, a
    `test/src/**` module is fixture semantics, a `test/tests/__init__.py`
    turns the tree into a package and changes rootdir-relative module names.
    """
    return (
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
) -> list[str]:
    """Judge a `test/**` edit line by line; `inert_only` drops the additive routes.

    `inert_only` is for the modules the collected tests *import* — prose may
    change, nothing else. A new `def`, a smoke marker and an `import pytest`
    are all meaningful additions to a collected test module and meaningless in
    a fixture module, where the same shapes would be new fixture behaviour
    arriving under the permit written for new tests.
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


def check_config_edit(repo: Path, base: str, head: str, path: str) -> list[str]:
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
            kept = _options(_replacement(text, hunk, hunks, path) or "")
            for opt in sorted(_options(text) - kept):
                problems.append(
                    f"{path}:-{lineno}: removed line drops the option `{opt}`, which the line that "
                    f"replaced it does not carry: {text.strip()!r}"
                )
    return problems


def _module_problems(repo: Path, head: str, path: str) -> list[str]:
    return [f"{path}:{p}" for p in module_rebinding_problems(git(repo, "show", f"{head}:{path}"))]


def check_range(
    repo: Path, base: str, head: str, allow: frozenset[tuple[str, int]]
) -> list[str]:
    """Every violation in `base..head` under `test/`, as `path:line: reason` strings."""
    problems: list[str] = []
    listing = git(repo, "diff", "--name-status", "--no-renames", base, head, "--", "test/")
    for row in listing.splitlines():
        status, _, path = row.partition("\t")
        if status == "D":
            problems.append(f"{path}: deleted — the suite never shrinks")
        elif path in ALLOWED_CONFIG:
            problems.extend(check_config_edit(repo, base, head, path))
        elif status == "A":
            if _new_file_allowed(path):
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
                problems.extend(check_python_edit(repo, base, head, path, allow))
            else:
                problems.extend(check_relocation_only(repo, base, head, path, allow))
        else:
            problems.append(f"{path}: unexpected git status {status!r}")
    return problems


# ---------------------------------------------------------------------------
# --self-test: one throwaway repository per shape
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
    _write_tree(repo, case.head)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "head")
    head = git(repo, "rev-parse", "HEAD").strip()
    # The mutation must have landed: a case whose head equals its base would
    # be green for the wrong reason.
    if not git(repo, "diff", "--name-only", base, head).split():
        raise SystemExit(f"self-test: {case.name}: head commit changed nothing — a green here would be vacuous")
    problems = check_range(repo, base, head, parse_allow(case.allow))
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


def self_test() -> int:
    scratch_root = REPO_ROOT / ".tmp"
    scratch_root.mkdir(exist_ok=True)
    failures = 0
    with tempfile.TemporaryDirectory(prefix="diff-guard-", dir=scratch_root) as tmp:
        for case in SELF_TEST_CASES:
            red, problems = _run_case(case, Path(tmp))
            ok = red == case.expect_red
            failures += not ok
            verdict = "red  " if red else "green"
            status = "ok  " if ok else "FAIL"
            print(f"{status} {verdict} {case.name}")
            if not ok:
                print(f"       expected {'red' if case.expect_red else 'green'}")
            for p in problems:
                print(f"       {p}")
        probes = _floor_probes(Path(tmp))
        for name, ok, detail in probes:
            failures += not ok
            print(f"{'ok  ' if ok else 'FAIL'} probe {name}")
            if not ok:
                print(f"       {detail}")
    total = len(SELF_TEST_CASES) + len(probes)
    print(f"self-test: {total - failures}/{total} shapes behave ({failures} wrong)")
    return 1 if failures else 0


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
    parser.add_argument("--self-test", action="store_true", help="show every shape red/green")
    ns = parser.parse_args(argv)
    if ns.self_test:
        return self_test()
    if not ns.range:
        parser.error("a <base>..<head> range or --self-test is required")
    base, head = split_range(ns.range)
    if "..." in ns.range:
        base = git(REPO_ROOT, "merge-base", base, head).strip()
    problems = check_range(REPO_ROOT, base, head, parse_allow(ns.allow))
    for p in problems:
        print(p)
    if problems:
        print(f"test-diff guard: {len(problems)} violation(s) in {ns.range} under test/", file=sys.stderr)
        return 1
    print(f"test-diff guard: {ns.range} changes nothing the acceptance suite asserts")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
