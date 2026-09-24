"""No acceptance test asserts on a Rust crate path (plan C-048, design A.5).

The acceptance suite is the oracle for the crate split: it must say the same
thing before and after ``ocx_lib`` dissolved into seventeen crates. A test that
names a Rust path in a *live* string — an assertion, a command argument, an
expected stderr fragment — stops being that oracle, because it reds on the move
rather than on a behaviour change, and a builder under a red suite reaches for
the test rather than the code.

Prose is exempt on purpose. A docstring or a comment naming ``ocx_setup::shims``
is the cheapest way to say where a fixture was copied from, it asserts nothing,
and forbidding it would cost the suite its only pointers back into the source.
So this guard reads the *parsed* module: comments never reach the AST at all,
and docstrings are dropped by position.

This is a structural test of the plan, not an acceptance assertion (DEC-10 f).
It exercises no ``ocx`` binary and needs no registry.
"""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

# The acceptance modules this sweep reads; it lives in the lint tier beside them.
TESTS_DIR = Path(__file__).resolve().parents[1] / "tests"
SELF = Path(__file__).resolve()

# The needles are built by concatenation, never written whole, so that this
# file cannot match itself and be excluded for the wrong reason. The
# self-exclusion below is the deliberate one; a literal here would make it
# look redundant.
_PREFIX = "ocx"
_SEP = ":" + ":"
LIB_NEEDLE = _PREFIX + "_lib" + _SEP
CRATE_PREFIX = _PREFIX + "_"

# A floor on the corpus. An `ast` walk that reached nothing would pass every
# assertion below in silence, which is the exact failure this guard exists to
# catch elsewhere.
MIN_MODULES = 50
MIN_LITERALS = 500


def crate_path_in(text: str) -> str | None:
    """The first ``ocx_<crate>::`` path in ``text``, or ``None``.

    Hand-rolled rather than a regex so the rule is readable at the call site:
    a crate prefix, at least one lowercase-or-underscore character, then the
    path separator. ``ocx::app`` (the CLI crate, no underscore) and
    ``ocx_lib=debug`` (an ``OCX_LOG`` directive, no separator) are not crate
    paths and are not forbidden — both appear in ``test_logging.py`` as live,
    correct values.
    """
    start = 0
    while (hit := text.find(CRATE_PREFIX, start)) != -1:
        cursor = hit + len(CRATE_PREFIX)
        while cursor < len(text) and (text[cursor].islower() or text[cursor] == "_"):
            cursor += 1
        if cursor > hit + len(CRATE_PREFIX) and text[cursor : cursor + 2] == _SEP:
            return text[hit : cursor + 2]
        start = hit + 1
    return None


def docstring_ids(tree: ast.AST) -> set[int]:
    """The ``id()`` of every string node that sits in docstring position."""
    found: set[int] = set()
    for node in ast.walk(tree):
        if not isinstance(
            node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)
        ):
            continue
        if not node.body:
            continue
        first = node.body[0]
        if (
            isinstance(first, ast.Expr)
            and isinstance(first.value, ast.Constant)
            and isinstance(first.value.value, str)
        ):
            found.add(id(first.value))
    return found


def live_literals(path: Path) -> list[tuple[int, str]]:
    """Every ``(lineno, value)`` string literal in ``path`` that is not a docstring.

    A parse failure is raised, never skipped: a module this cannot read is a
    module this does not guard, and a guard that quietly covers less than it
    claims is the defect it is here to prevent.
    """
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    skip = docstring_ids(tree)
    return [
        (node.lineno, node.value)
        for node in ast.walk(tree)
        if isinstance(node, ast.Constant)
        and isinstance(node.value, str)
        and id(node) not in skip
    ]


def modules() -> list[Path]:
    return sorted(p for p in TESTS_DIR.glob("*.py") if p.resolve() != SELF)


def test_the_matcher_sees_a_crate_path_and_spares_the_near_misses() -> None:
    """The positive control: the matcher is shown catching and sparing.

    Without it a matcher that had stopped matching anything would make every
    module below pass for a reason unrelated to the property.
    """
    caught = _PREFIX + "_lib" + _SEP + "setup::POSIX_BODY"
    assert crate_path_in(caught) == LIB_NEEDLE
    assert (
        crate_path_in("a " + _PREFIX + "_store" + _SEP + "b")
        == _PREFIX + "_store" + _SEP
    )

    # Near misses that are live, correct values elsewhere in this suite.
    assert crate_path_in(_PREFIX + _SEP + "app") is None
    assert crate_path_in(_PREFIX + "_lib=debug") is None
    assert crate_path_in(_PREFIX + "_lib") is None
    assert crate_path_in("OCX_LOG") is None


def test_the_walk_reaches_the_whole_suite() -> None:
    """The corpus is non-empty, so the sweep below asserts something."""
    found = modules()
    assert len(found) >= MIN_MODULES, (
        f"only {len(found)} test modules under {TESTS_DIR} — the walk found almost "
        "nothing, so it scanned nothing"
    )
    assert SELF not in {p.resolve() for p in found}, "this file must exclude itself"

    total = sum(len(live_literals(path)) for path in found)
    assert total >= MIN_LITERALS, (
        f"only {total} live string literals across {len(found)} modules — the parse "
        "reached almost nothing, so every assertion over it is vacuous"
    )


@pytest.mark.parametrize("module", modules(), ids=lambda p: p.name)
def test_no_module_names_a_rust_crate_path_in_a_live_string(module: Path) -> None:
    """No live string literal spells a Rust crate path (C-048 / DEC-3).

    Comments and docstrings are exempt and are dropped before this runs: name
    the source freely in prose, never in a value the suite acts on.
    """
    offenders = [
        f"{module.name}:{lineno}: {hit} in {value!r}"
        for lineno, value in live_literals(module)
        if (hit := crate_path_in(value)) is not None
    ]
    assert not offenders, (
        "a live string literal names a Rust crate path, so this test reds when the "
        "crate split moves the module rather than when ocx changes behaviour — move "
        "the spelling into a comment or a docstring, or assert on observable output "
        "instead:\n  " + "\n  ".join(offenders)
    )
