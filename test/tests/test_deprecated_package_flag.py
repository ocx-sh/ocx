"""C-062's repo-wide structural check for the deprecated ``--package`` flag.

``ocx package announce`` took ``--package <PACKAGE>`` until the positional
replaced it.  The flag survives as a hidden argument for one release pair, so
nothing in this repository may keep *using* it — ocx would otherwise instruct
an operator to run a form ocx itself warns about, and at 0.7 a form that does
not exist.

The contract is C-062, quoted here verbatim rather than paraphrased, because a
check whose predicate is restated in the author's own words gets written to
whatever makes it green:

    **The predicate, stated so the check cannot be written green.**
    "Invocation" means a *rendered ``ocx package announce`` command line
    carrying ``--package``* -- which exempts prose that discusses the flag
    (``test/manual/announce-e2e/CLAIM.md``) and the one explanatory comment in
    ``crates/ocx_cli/src/command/package_cascade_repair.rs``, and catches
    format strings, shell scripts, YAML and markdown code fences alike.  Scope
    is **repo-wide**, excluding ``.git``, ``target``, ``external``,
    ``.agents``, ``node_modules``, ``website/.vitepress/dist`` (build output,
    gitignored) and ``.claude/artifacts/**`` + ``.claude/state/**``
    (historical records that must not be rewritten).  Exactly one dedicated
    test retains the hidden ``--package`` form and is the check's sole allowed
    hit.  The check is **red-proved by adding a tenth invocation to a scratch
    file and observing it fail**, then removing it.

Two renderings of "a rendered command line" exist in this tree, and the check
decides each one mechanically -- see :data:`_SPELLED_RE` and :data:`_ARGV_RE`
for the rule and for what each one deliberately does *not* match.

No Docker, no built binary, no registry: pure static file analysis.
"""

from __future__ import annotations

import os
import re
from pathlib import Path

from src.helpers import PROJECT_ROOT

# ---------------------------------------------------------------------------
# Scope (C-062's exclusion set, one entry per named exclusion)
# ---------------------------------------------------------------------------

_EXCLUDED_DIR_NAMES: frozenset[str] = frozenset(
    {".git", "target", "external", ".agents", "node_modules"}
)
"""Directory names pruned wherever they appear, from C-062's exclusion set."""

_EXCLUDED_PATHS: tuple[str, ...] = (
    "website/.vitepress/dist",
    ".claude/artifacts",
    ".claude/state",
    ".tmp",
    "out",
)
"""Repo-relative subtrees pruned, from C-062's exclusion set.

``website/.vitepress/dist`` is gitignored build output; ``.claude/artifacts``
and ``.claude/state`` are historical records that must not be rewritten.

``.tmp`` is gitignored scratch, pruned for the same reason as the build output
and additionally because this check's *own* failure message renders a matching
command line: a run that reports a hit writes that hit into the verify log,
and scanning the log would keep the check red forever after its first true
positive. C-062 named the exclusions it knew about; this is the fourth.

``out`` is the same class as ``.tmp``: gitignored scratch this repository's own
tasks write into. ``task claude:review-surface`` renders a branch diff to
``out/review-branch.html``, so any branch that *removes* a matching command line
leaves one rendered in that page and turns the sweep red against a file no
commit contains.
"""

SOLE_ALLOWED_HIT: Path = PROJECT_ROOT / "test" / "tests" / "test_announce.py"
"""The one dedicated test that retains the hidden form (C-062).

``test_announce.py::test_announce_hidden_package_flag_warns_once`` invokes the
deprecated spelling for real, because the deprecation window's own behaviour --
it still executes, it warns once, the notice never reaches stdout -- has no
other way to be observed.
"""

# Held on two lines, and every pattern below is assembled from them with
# `re.escape` rather than written out. A guard whose needle is a literal in the
# file it scans measures itself: spelling the command and the flag together in
# one source line here would make this module its own first hit, in every state.
_DEPRECATED_FLAG = "--package"
_COMMAND = "ocx package announce"

# ---------------------------------------------------------------------------
# The two renderings
# ---------------------------------------------------------------------------

_SPELLED_RE: re.Pattern[str] = re.compile(
    re.escape(_COMMAND) + r".*" + re.escape(_DEPRECATED_FLAG)
)
"""SPELLED -- the text names the command it runs.

Applied to a line with every backtick-delimited span removed first.  That is
what separates an invocation from prose: a markdown code fence, a shell
script, a YAML ``run:`` step and a format string all spell the command bare,
while every place C-062 exempts by name discusses the flag *inside backticks*
(``CLAIM.md``, the ``package_cascade_repair.rs`` comment, the deprecation
notice ocx prints, and the reference page's own prose about the flag).
"""

_ARGV_RE: re.Pattern[str] = re.compile(
    r"([\"'])" + re.escape(_DEPRECATED_FLAG) + r"\1\s*,"
)
"""ARGV -- the flag is an element of an argument list handed to a process.

Scoped to ``test/`` (see :func:`_rendering_applies`), where the acceptance
helpers spawn a real ``ocx package announce`` without spelling the command in
the source line.  The trailing comma is the discriminator: an argument is
followed by its value, so ``"--package", package`` matches, while an assertion
needle -- ``assert "--package" not in result.stdout`` -- does not.

It does **not** apply under ``crates/``: a clap unit test's
``try_parse_from(["announce", ..., "--package", ...])`` never renders
``ocx package announce`` and never spawns anything, and C-062's own inventory
of the sweep counts no ``package_announce.rs`` line among its invocations.
"""

_CONTINUED_RE: re.Pattern[str] = re.compile(r"\\\s*$")
"""A physical line that continues onto the next one.

Shell, `Makefile`s, YAML ``run:`` blocks and markdown fences all break a long
command line with a trailing backslash, and both renderings above are decided
**per line** -- so before this was folded, the single most common way a
documented invocation is written in this tree was the one shape the sweep could
not see, and it reported clean over text it never examined.

A line ending in an escaped backslash (``\\\\``) is folded too, which is a
deliberate over-inclusion: the failure it can produce is a false hit, which is
loud and is read by a human, where the failure it prevents is the silent one.
"""

_BACKTICKED_RE: re.Pattern[str] = re.compile(r"`+[^`]*`+")
"""A backtick-delimited span, in any of the markup this tree uses.

Runs of backticks, not a single pair: markdown inline code writes `` `x` `` and
reStructuredText inline literals -- which every docstring here uses, including
this module's own quotation of C-062 -- write ``` ``x`` ```.  Stripping only
the single-backtick form would leave this file as its own first hit.
"""

_TEST_TREE: Path = PROJECT_ROOT / "test"


def _rendering_applies(path: Path) -> bool:
    """Whether the ARGV rendering is decidable for ``path``."""
    return path.is_relative_to(_TEST_TREE)


def _is_text(path: Path) -> bool:
    """Whether ``path`` is text, by the classic NUL-byte sniff.

    A suffix allowlist would silently narrow C-062's "repo-wide" scope every
    time someone adds a file type; a NUL sniff only ever skips a file no
    command line could be written in.
    """
    try:
        with path.open("rb") as handle:
            head = handle.read(8192)
    except OSError:
        return False
    return b"\0" not in head


def _scan_files() -> list[Path]:
    """Every in-scope file, after C-062's exclusion set is applied."""
    found: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(PROJECT_ROOT):
        here = Path(dirpath)
        dirnames[:] = [
            name
            for name in dirnames
            if name not in _EXCLUDED_DIR_NAMES
            and str((here / name).relative_to(PROJECT_ROOT)).replace(os.sep, "/")
            not in _EXCLUDED_PATHS
        ]
        found.extend(here / name for name in filenames)
    return found


def _logical_lines(text: str) -> list[tuple[int, str]]:
    """``text`` as ``(first physical line number, logical line)`` pairs.

    A run of physical lines joined by trailing backslashes becomes one logical
    line, reported against the number of the line it *starts* on -- which is
    where a reader looks for the invocation, and where the command is spelled.

    Continued fragments are joined with a single space rather than concatenated:
    the fragments carry their own leading indentation, and a rendering's ``.*``
    must see one line's worth of separators, not the file's layout.
    """
    lines: list[tuple[int, str]] = []
    pending: list[str] = []
    start = 1
    for number, line in enumerate(text.splitlines(), start=1):
        if not pending:
            start = number
        if _CONTINUED_RE.search(line):
            pending.append(_CONTINUED_RE.sub("", line).strip())
            continue
        pending.append(line.strip())
        lines.append((start, " ".join(part for part in pending if part)))
        pending = []
    if pending:
        lines.append((start, " ".join(part for part in pending if part)))
    return lines


def hits_in_text(text: str, *, argv_applies: bool) -> list[tuple[int, str]]:
    """Every rendered invocation in ``text``, as ``(line number, line)`` pairs.

    Split out from :func:`find_invocations` so the matching rule can be driven
    against text a test authors, not only against the tree as it happens to
    stand: a sweep whose red state is only reachable by dropping a file into the
    repository has no permanent negative control at all.
    """
    hits: list[tuple[int, str]] = []
    for number, line in _logical_lines(text):
        if _DEPRECATED_FLAG not in line:
            continue
        # Both renderings judge the line with its backtick-delimited spans
        # removed: quoting either form inside markup is the flag being
        # discussed, which C-062 exempts, and a real invocation is never
        # written inside backticks.
        bare = _BACKTICKED_RE.sub("", line)
        if _SPELLED_RE.search(bare) or (argv_applies and _ARGV_RE.search(bare)):
            hits.append((number, line))
    return hits


def find_invocations() -> list[tuple[Path, int, str]]:
    """Every rendered ``ocx package announce`` command line carrying the flag.

    Returns:
        ``(path, line number, line)`` triples, line numbers 1-based.
    """
    hits: list[tuple[Path, int, str]] = []
    for path in _scan_files():
        if not _is_text(path):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        if _DEPRECATED_FLAG not in text:
            continue
        hits.extend(
            (path, number, line)
            for number, line in hits_in_text(text, argv_applies=_rendering_applies(path))
        )
    return hits


def test_no_package_flag_invocation_survives_outside_the_deprecation_test() -> None:
    """C-062: the sweep is complete, and one dedicated test still proves the
    deprecated spelling executes.

    Both halves are asserted.  The negative alone is green when the deprecation
    test is deleted -- indistinguishable from a check that never ran -- so the
    positive control (the allowed file *does* still invoke the flag) is what
    makes the negative mean anything.
    """
    hits = find_invocations()

    stray = [hit for hit in hits if hit[0] != SOLE_ALLOWED_HIT]
    assert stray == [], (
        "`ocx package announce --package` is deprecated and removed in 0.7; "
        "these rendered invocations must move to the positional form:\n"
        + "\n".join(
            f"  {path.relative_to(PROJECT_ROOT)}:{number}: {line}"
            for path, number, line in stray
        )
    )

    allowed = [hit for hit in hits if hit[0] == SOLE_ALLOWED_HIT]
    assert allowed, (
        f"{SOLE_ALLOWED_HIT.relative_to(PROJECT_ROOT)} no longer invokes the "
        "deprecated `--package` spelling, so this check can no longer tell a "
        "complete sweep from a check that never ran. Restore the deprecation "
        "test, or retire this check together with the flag."
    )


def test_a_continuation_split_invocation_is_visible_to_the_sweep() -> None:
    """The sweep folds backslash continuations, and that fold is what sees a
    wrapped invocation.

    The permanent negative control for the row above.  C-062 asks for the red
    state to be reached "by adding a tenth invocation to a scratch file", which
    is a red nobody can reach twice: the file is removed, and the next reader
    inherits a green with no evidence behind it.  This reaches the same red from
    text the test authors, so it stays reachable.

    Both samples are assembled from :data:`_COMMAND` and
    :data:`_DEPRECATED_FLAG` rather than written out, for the reason those two
    constants exist: spelling the command and the flag together in one source
    line would make this module its own first hit, in every state.

    The second assertion is the discriminator.  Without it the first passes for
    a sweep that matched the two renderings across the *whole file* -- which
    would call any two unrelated lines an invocation -- so the same two lines
    with the backslash removed must stay two command lines and match nothing.
    """
    continued = f"{_COMMAND} \\\n    {_DEPRECATED_FLAG} acme/widget\n"
    separate = f"{_COMMAND}\n{_DEPRECATED_FLAG} acme/widget\n"

    assert hits_in_text(continued, argv_applies=False) == [
        (1, f"{_COMMAND} {_DEPRECATED_FLAG} acme/widget")
    ], (
        "a wrapped invocation was invisible, or was reported against the line "
        "carrying the flag rather than the line the command starts on"
    )
    assert hits_in_text(separate, argv_applies=False) == [], (
        "two separate command lines were read as one: the fold is joining lines "
        "no backslash continued, and the sweep now reports invocations nobody wrote"
    )
