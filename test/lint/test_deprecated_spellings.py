"""Repo-wide structural check for every spelling the deprecation window retires.

Three kinds of spelling are retired at once, and all are swept here:

* the renamed **flag** (C-062) — ``ocx package announce`` took ``--package
  <PACKAGE>`` until the positional replaced it;
* every renamed **command** and **flag**, read out of the ``deprecated`` field of
  the published ``cli.json`` so the Rust side is the single authority;
* every renamed **environment variable**, out of the in-window ``retired`` rows
  beside it, stale wherever a whole token names it outside a declaring file.

Nothing in this repository may keep *using* an old spelling — ocx would
otherwise instruct an operator to run a form ocx itself warns about, and at the
removal release a form that does not exist.

The flag half's contract is C-062, quoted verbatim rather than paraphrased,
because a check whose predicate is restated in the author's own words gets
written to whatever makes it green:

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

C-062 enumerated its exclusions as directory names because it was written
against an ``os.walk``.  That walk is gone -- see :func:`_scan_files` -- and
with it every exclusion but one: the file list now comes from git, so
``.gitignore`` maintains the set and no future cache directory can re-enter
scope by nobody having thought of it.

Three renderings of "a rendered command line" exist in this tree, and the check
decides each one mechanically -- see :data:`_SPELLED_RE`, :data:`_ARGV_RE` and
:func:`_rename_renderings` for the rule and for what each one deliberately does
*not* match.
"""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Any, NamedTuple

from src.helpers import PROJECT_ROOT

# ---------------------------------------------------------------------------
# Scope
# ---------------------------------------------------------------------------

_EXCLUDED_PATHS: tuple[str, ...] = (".claude/artifacts",)
"""Repo-relative subtrees pruned. Exactly one, and it earns its place.

``.claude/artifacts`` holds historical records -- plans and analyses that quote
the command lines of the day -- which must not be rewritten to match today's
grammar.  They are *tracked*, so nothing else drops them.

The other four exclusions C-062 named (``website/.vitepress/dist``,
``.claude/state``, ``.tmp``, ``out``) are gitignored build output and scratch,
and are gone from this tuple because :func:`_scan_files` no longer sees them --
verified, not assumed.  ``.tmp`` and ``out`` were the subtle two: this check's
own failure message renders a matching command line, and both
``task claude:review-surface``'s rendered diff page and the verify log land
there, so a run that reported a true positive used to poison the next one.
``--exclude-standard`` covers all four for free, and covers the next one nobody
thought of as well.
"""

SOLE_ALLOWED_HIT: dict[str, Path] = {
    # Keyed by spelling, because each deprecated form needs its own positive
    # control: one exemption vouching for a different spelling's sweep would
    # make that sweep's green indistinguishable from a check that never ran.
    "--package": PROJECT_ROOT / "test" / "tests" / "test_announce.py",
}
"""Spelling -> the one dedicated test that retains it (C-062).

``test_announce.py::test_deprecated_package_flag_warns_once_on_stderr_only``
invokes the deprecated flag for real, because the deprecation window's own
behaviour -- it still executes, it warns once, the notice never reaches
stdout -- has no other way to be observed.

**A rename with no dedicated behavioural test gets no entry.** None of the
renamed commands has one today, so none is exempt anywhere, and the sweep over
them is a plain negative.  Adding an entry here is how a future behavioural
test declares itself; adding one without such a test would hand a stray
invocation a permanent hiding place.
"""

# Held on two lines, and every pattern below is assembled from them with
# `re.escape` rather than written out. A guard whose needle is a literal in the
# file it scans measures itself: spelling the command and the flag together in
# one source line here would make this module its own first hit, in every state.
# The same rule is why no renamed command spelling appears anywhere in this
# file: they are read from `cli.json` at run time.
_DEPRECATED_FLAG = "--package"
_COMMAND = "ocx package announce"

_CLI_JSON = PROJECT_ROOT / "crates" / "ocx_schema" / "tests" / "golden" / "cli.json"
"""The committed ``cli.json`` -- the golden the schema tests hold equal to what ocx generates.

``website/src/public/schemas`` is gitignored build output, absent from a fresh checkout, so the
golden is the one copy of the published document this tier can always read.
"""


def _published_cli() -> dict[str, Any]:
    document = json.loads(_CLI_JSON.read_text(encoding="utf-8"))
    assert document["root"]["commands"], "cli.json lists no commands, so the sweep has no input"
    return document


def renamed_spellings() -> tuple[tuple[str, str], ...]:
    """Every deprecated command and flag spelling as ``(old, new)``, commands first.

    Read out of the ``deprecated`` field ``cli.json`` publishes on a command or an
    argument, rather than restated: the Rust rows are the single authority, and a rename
    that opens a window cannot be forgotten on this side.  A flag's old spelling is its
    command path plus the spelling as typed (``package push -c``), its new one the
    published ``replacement``.  The Rust side guards its own half --
    ``deprecated::tests`` count the dispatch sites against the rows -- so the two halves
    together close the loop from "a command warns" to "no invocation of it survives".
    """
    commands: list[tuple[str, str]] = []
    flags: list[tuple[str, str]] = []

    def walk(command: dict[str, Any]) -> None:
        path = command["path"]
        if "deprecated" in command:
            commands.append((" ".join(path), command["deprecated"]["replacement"]))
        for arg in command["args"]:
            if "deprecated" in arg:
                spelling = f"--{arg['long']}" if "long" in arg else f"-{arg['short']}"
                flags.append((" ".join([*path, spelling]), arg["deprecated"]["replacement"]))
        for sub in command["commands"]:
            walk(sub)

    walk(_published_cli()["root"])
    assert commands and flags, "cli.json publishes no deprecated command or flag — the sweep would report clean over nothing"
    return (*commands, *flags)


def renamed_env_spellings() -> tuple[tuple[str, str], ...]:
    """The in-window rows of ``retired`` in ``cli.json``, as ``(old, new)`` variable names.

    A row whose ``status`` is ``window`` is a variable ocx still honours and warns about, so
    it is one this sweep looks for; a ``removed`` row is refused outright and is not.  A
    ``value_rename`` row would need the old *value* to sweep for, which the published row
    does not carry, so it fails here rather than being swept as its bare name.
    """
    rows = [row for row in _published_cli()["retired"] if row["status"] == "window"]
    assert rows, "cli.json publishes no in-window retired row — the environment sweep would report clean over nothing"
    unsupported = [row["name"] for row in rows if row["change"] == "value_rename"]
    assert not unsupported, f"cli.json does not carry the old value of {unsupported}; publish it before sweeping for it"
    return tuple((row["name"], row["replacement"]) for row in rows)


ENV_DECLARATIONS: frozenset[Path] = frozenset(
    {
        PROJECT_ROOT / "crates" / "ocx_env" / "src" / "retired.rs",
        PROJECT_ROOT / "crates" / "ocx_schema" / "tests" / "golden" / "cli.json",
        PROJECT_ROOT / "CLAUDE.md",
    }
)
"""Files that name an old environment spelling to declare the rename, not to use it."""

ENV_WINDOW_TEST: Path = PROJECT_ROOT / "test" / "tests" / "test_env_contract.py"
"""The acceptance module that sets every old environment spelling for real.

It proves the window's behaviour -- the old name is still honoured and warns
once on stderr -- so it must keep each spelling, and it is the sweep's positive
control: every in-window ``retired`` spelling must still hit here.
"""


def env_hits_in_text(text: str, old: str) -> list[tuple[int, str]]:
    """Every line of ``text`` naming ``old`` as a whole token."""
    token = re.compile(r"(?<![A-Za-z0-9_])" + re.escape(old) + r"(?![A-Za-z0-9_])")
    return [
        (number, line.strip())
        for number, line in enumerate(text.splitlines(), start=1)
        if token.search(line)
    ]


def find_env_spellings() -> list[Hit]:
    """Every whole-token occurrence of every renamed environment spelling."""
    renamed = renamed_env_spellings()
    hits: list[Hit] = []
    for path in _scan_files():
        if not _is_text(path):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        for old, _new in renamed:
            if old not in text:
                continue
            hits.extend(Hit(old, path, number, line) for number, line in env_hits_in_text(text, old))
    return hits


# ---------------------------------------------------------------------------
# The renderings
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

#: Where a command may begin: the start of a logical line, after a shell
#: operator, after a prompt, or after a YAML ``run:`` key.
_COMMAND_POSITION = r"(?:^|[|;&(]\s*|\$\s+|run:\s*)"


#: Root options between ``ocx`` and its command: a flag, and the value it may take.
_ROOT_OPTIONS = r"(?:\s+-\S+(?:\s+[^-\s]\S*)?)*"

#: Where a flag ends: whitespace, ``=`` (``--flag=value``), or the end of the line.
_FLAG_END = r"(?=[\s=]|$)"


def _flag_renderings(command: str, flag: str) -> tuple[re.Pattern[str], re.Pattern[str]]:
    """``(spelled, argv)`` patterns for one renamed flag of ``command``.

    SPELLED: the command at a command position, after any root options, then the
    flag on the same logical line short of a shell operator.  A flag before the
    command is the root's own; a longer flag on the same prefix is another flag.

    ARGV: the command's words adjacent *and* the quoted flag later on the line; a
    bare ``"-c",`` is ``sh -c`` throughout ``test/``.  A helper-assembled argv
    stays invisible, the one-word rename's trade.
    """
    spelled = re.compile(
        _COMMAND_POSITION
        + r"ocx"
        + _ROOT_OPTIONS
        + r"\s+"
        + re.escape(command)
        + r"\s(?:[^|;&]*\s)?"
        + re.escape(flag)
        + _FLAG_END
    )
    words = command.split()
    adjacent = r"\s*,\s*".join(f"([\"']){re.escape(word)}\\{index + 1}" for index, word in enumerate(words))
    argv = re.compile(adjacent + r"[^)]*?[\"']" + re.escape(flag) + r"[\"']")
    return spelled, argv


def _rename_renderings(old: str) -> tuple[re.Pattern[str], re.Pattern[str] | None]:
    """``(spelled, argv)`` patterns for one renamed command spelling.

    **SPELLED** is the flag rendering's rule with a different discriminator.
    The flag half could ask for the command *and* the flag on one line, which no
    sentence writes by accident.  A renamed command has no second token to
    demand, and ``ocx <word>`` is a phrase English prose writes constantly --
    "a later ocx run against this registry" is a sentence, not an invocation.
    What separates them is **position**: an invocation starts a command, so the
    spelling must sit at the start of the logical line, after a shell operator
    (``|``, ``;``, ``&&``), after a ``$`` prompt, or after a YAML ``run:``.

    That is deliberately over-inclusive at the margin -- prose that happens to
    begin a wrapped line with the old spelling is reported.  Same trade the
    continuation fold takes below, for the same reason: the failure it can
    produce is a false hit, which is loud and is read by a human, where the
    failure it prevents is the silent one.  The remedy for a false hit is to
    reword the sentence or put the spelling in backticks, which is where prose
    discussing a command belongs anyway.

    **ARGV** exists only for a multi-word spelling, and that is not a gap being
    tolerated -- it is the rendering being undecidable for a single word.  A
    two-word spelling reaches a process as *adjacent* argv elements, and the
    adjacency is the whole discriminator; a one-word spelling reaches it as a
    bare string that this tree writes for a hundred unrelated reasons (a
    ``RUST_LOG`` level, a dict key, a fixture id), so a pattern for it would
    report noise on every run and be silenced within a week.  A one-word
    invocation is still caught by SPELLED wherever the command is spelled --
    which is every shell script, every doc fence, and every YAML step.
    """
    command, _, flag = old.rpartition(" ")
    if command and flag.startswith("-"):
        return _flag_renderings(command, flag)
    spelled = re.compile(_COMMAND_POSITION + r"ocx " + re.escape(old) + r"(?:\s|$)")
    words = old.split()
    if len(words) < 2:
        return spelled, None
    adjacent = r"\s*,\s*".join(f"([\"']){re.escape(word)}\\{index + 1}" for index, word in enumerate(words))
    return spelled, re.compile(adjacent)


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
    """Every in-scope file, from git rather than from a directory walk.

    ``git ls-files --cached --others --exclude-standard`` is the tracked set
    plus the untracked-and-not-ignored set: exactly "files this repository is
    responsible for", which is what C-062's "repo-wide" means and what its
    hand-written exclusion list was approximating.

    The walk this replaces was a partial re-implementation of ``.gitignore``,
    and it failed the way partial re-implementations fail -- silently, in the
    direction of doing *more*.  It pruned five directory names it knew about
    and read everything else, which came to **91,217 files / 4.3 GB**, most of
    it a cross-compilation cache (``.cache/xwin``) nobody had thought of. Warm
    that cost 3 s; cold, against 32 xdist workers competing for the same disk,
    it cost 128.65 s -- 74 % of the entire acceptance suite's wall clock, in one
    test. The git query costs 4 ms and returns 2,210 files.

    It is also the *correct* set rather than a cheaper approximation of it: a
    new build-output or cache directory is ignored by the same rule that keeps
    it out of ``git status``, so no future one can re-enter this sweep's scope
    by nobody having remembered to add it here.
    """
    listed = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        check=True,
    )
    names = [name for name in listed.stdout.split("\0") if name]
    assert names, "git listed no files at all — the sweep would report clean over nothing"
    return [
        PROJECT_ROOT / name
        for name in names
        if not any(name.startswith(f"{excluded}/") for excluded in _EXCLUDED_PATHS)
    ]


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
    """Every rendered flag invocation in ``text``, as ``(line number, line)``.

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


def rename_hits_in_text(text: str, old: str, *, argv_applies: bool) -> list[tuple[int, str]]:
    """Every rendered invocation of the renamed command ``old`` in ``text``.

    The rename counterpart of :func:`hits_in_text`, and split out for the same
    reason: :func:`test_the_rename_renderings_separate_an_invocation_from_prose`
    drives it against authored text, so its red state stays reachable after the
    tree is clean.
    """
    spelled, argv = _rename_renderings(old)
    hits: list[tuple[int, str]] = []
    for number, line in _logical_lines(text):
        bare = _BACKTICKED_RE.sub("", line)
        if spelled.search(bare) or (argv_applies and argv is not None and argv.search(bare)):
            hits.append((number, line))
    return hits


class Hit(NamedTuple):
    """One rendered invocation of one deprecated spelling."""

    spelling: str
    path: Path
    number: int
    line: str


def find_invocations() -> list[Hit]:
    """Every rendered invocation of every deprecated spelling in the tree."""
    renamed = renamed_spellings()
    hits: list[Hit] = []
    for path in _scan_files():
        if not _is_text(path):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        argv_applies = _rendering_applies(path)
        if _DEPRECATED_FLAG in text:
            hits.extend(
                Hit(_DEPRECATED_FLAG, path, number, line)
                for number, line in hits_in_text(text, argv_applies=argv_applies)
            )
        for old, _new in renamed:
            # Cheap pre-filter, exactly as the flag half has: the expensive part
            # is the per-line fold, and most files carry the spelling nowhere.
            # Every *word*, not the spelling: ARGV renders a two-word spelling as
            # `"package", "describe"`, in which the spelling itself never appears
            # as a substring. A `old not in text` filter therefore skipped every
            # argv-rendered invocation in the tree while reporting clean, and the
            # sweep's own day-one red state is what surfaced it.
            if not all(word in text for word in old.split()):
                continue
            hits.extend(
                Hit(old, path, number, line)
                for number, line in rename_hits_in_text(text, old, argv_applies=argv_applies)
            )
    return hits


def test_no_deprecated_spelling_survives_outside_its_own_deprecation_test() -> None:
    """The sweep is complete, and every exempted spelling is still exercised.

    Both halves are asserted.  The negative alone is green when the deprecation
    test is deleted -- indistinguishable from a check that never ran -- so the
    positive control (the allowed file *does* still invoke that spelling) is
    what makes the negative mean anything.  The control is per spelling: a
    single "some exemption still fires" would let one live deprecation test
    vouch for every other spelling's sweep.
    """
    renamed = dict(renamed_spellings())
    hits = find_invocations()

    stray = [hit for hit in hits if hit.path != SOLE_ALLOWED_HIT.get(hit.spelling)]
    assert stray == [], (
        "these rendered invocations use a spelling this deprecation window retires; move each one "
        "to the form that replaces it:\n"
        + "\n".join(
            f"  {hit.path.relative_to(PROJECT_ROOT)}:{hit.number}: [{hit.spelling}"
            + (f" -> {renamed[hit.spelling]}" if hit.spelling in renamed else "")
            + f"] {hit.line}"
            for hit in stray
        )
    )

    for spelling, allowed in SOLE_ALLOWED_HIT.items():
        assert any(hit.spelling == spelling and hit.path == allowed for hit in hits), (
            f"{allowed.relative_to(PROJECT_ROOT)} no longer invokes the deprecated `{spelling}` "
            "spelling, so this check can no longer tell a complete sweep from a check that never "
            "ran. Restore the deprecation test, or retire the exemption together with the spelling."
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


def test_the_rename_renderings_separate_an_invocation_from_prose() -> None:
    """The rename half's permanent control, in both directions and for both
    renderings.

    Every sample is assembled from a spelling read out of ``cli.json``, for
    the reason the constants above exist: writing one out would make this module
    its own first hit.

    Four claims, and the tree cannot supply any of them once it is clean:

    * a shell line that *starts* with the old spelling is an invocation;
    * the same words inside an English sentence are not -- position is the whole
      discriminator, and without this assertion the SPELLED rule could be
      loosened to a bare substring search and stay green;
    * a YAML ``run:`` step is an invocation, which is the shape a workflow uses
      and the one a line-start-only rule would miss;
    * a multi-word spelling handed to a process as adjacent argv elements is an
      invocation, and is invisible to SPELLED because no line names the command.
    """
    old, _new = renamed_spellings()[0]
    multi = next((pair[0] for pair in renamed_spellings() if " " in pair[0]), None)
    assert multi is not None, "no multi-word rename left — the ARGV rendering is unreachable"

    assert rename_hits_in_text(f"ocx {old} --help\n", old, argv_applies=False), (
        "a command line starting with the old spelling must be seen"
    )
    assert rename_hits_in_text(f"  run: ocx {old} -- tool\n", old, argv_applies=False), (
        "a YAML `run:` step is a rendered command line too"
    )
    assert rename_hits_in_text(f"a later ocx {old} against this registry leaves a rule\n", old, argv_applies=False) == [], (
        "prose that merely contains the words was reported: the position rule is gone and the "
        "sweep now reds on sentences"
    )

    argv = ", ".join(f'"{word}"' for word in multi.split())
    assert rename_hits_in_text(f"    ocx.plain({argv}, path)\n", multi, argv_applies=True), (
        "an argv-rendered invocation under test/ must be seen"
    )
    assert rename_hits_in_text(f"    ocx.plain({argv}, path)\n", multi, argv_applies=False) == [], (
        "the ARGV rendering fired outside the tree it is decidable in"
    )


def test_the_flag_renderings_separate_an_invocation_from_its_neighbours() -> None:
    """The flag rows' permanent control, both directions, both renderings.

    Rows are invented, so no real old spelling sits beside its command in this
    file.  One short and one long, each with its own neighbour: a short letter
    the root also binds, a long flag its replacement extends.
    """
    for command, flag, new in (("tool make", "-x", "--extra"), ("tool make", "--out", "--output")):
        old = f"{command} {flag}"

        def hits(text: str, *, argv: bool = False, old: str = old) -> list[tuple[int, str]]:
            return rename_hits_in_text(text, old, argv_applies=argv)

        assert hits(f"ocx {command} -i x {flag} y\n"), f"`{old}` after another option must be seen"
        assert hits(f"ocx --format json {command} {flag}=y\n"), "root options before the command hid it"
        assert hits(f"ocx {flag} y {command} z\n") == [], "a flag before the command is the root's own"
        assert hits(f"ocx {command} {new} y\n") == [], f"the replacement `{new}` was read as `{flag}`"
        assert hits(f"ocx {command} y | grep {flag}\n") == [], "a flag past a pipe belongs to another command"

        argv = ", ".join(f'"{word}"' for word in command.split())
        assert hits(f'    ocx.json({argv}, "{flag}", y)\n', argv=True), "an argv invocation under test/ must be seen"
        assert hits(f'    run(["sh", "{flag}", script])\n', argv=True) == [], "a bare quoted flag names no command"


def test_no_renamed_environment_spelling_survives_outside_its_declarations() -> None:
    """No file uses an environment spelling this window renames, and the window test still does.

    The negative alone is green when the window test is deleted, so the
    positive control -- every spelling still set in :data:`ENV_WINDOW_TEST` --
    is what makes it mean anything.
    """
    renamed = dict(renamed_env_spellings())
    hits = find_env_spellings()

    stray = [hit for hit in hits if hit.path not in ENV_DECLARATIONS and hit.path != ENV_WINDOW_TEST]
    assert stray == [], (
        "these lines name an environment spelling this deprecation window renames; use the new name:\n"
        + "\n".join(
            f"  {hit.path.relative_to(PROJECT_ROOT)}:{hit.number}: [{hit.spelling} -> "
            f"{renamed[hit.spelling]}] {hit.line}"
            for hit in stray
        )
    )

    for old in renamed:
        assert any(hit.spelling == old and hit.path == ENV_WINDOW_TEST for hit in hits), (
            f"{ENV_WINDOW_TEST.relative_to(PROJECT_ROOT)} no longer sets the renamed `{old}`, so this "
            "sweep cannot tell a complete sweep from one that never ran. Restore its window test."
        )


def test_the_env_token_rule_separates_the_old_spelling_from_its_neighbours() -> None:
    """The environment sweep's permanent control, built from the parsed spelling.

    A whole-token match: the old name in a shell export and a Python dict both
    hit; the new name, and a longer variable that merely contains the old one,
    do not -- a replacement can be a prefix of its old name, so a bare
    substring rule would read every use of one name as a use of the other.
    """
    old, new = renamed_env_spellings()[0]

    assert env_hits_in_text(f"export {old}=1\n", old) == [(1, f"export {old}=1")]
    assert env_hits_in_text(f'    env={{"{old}": "1"}}\n', old), "a dict key is a use"
    assert env_hits_in_text(f"export {new}=1\n", old) == [], "the replacement was read as the old name"
    assert env_hits_in_text(f"{old}_EXTRA=1 X_{old}=1\n", old) == [], "a longer name was read as the old one"
