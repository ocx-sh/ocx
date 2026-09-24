"""Traceability of the shell edge-case register (`analysis_shell_env_edge_cases.md`).

Every register row names a test that proves it, in pytest or in a Rust
`#[test]`; every test in `tests/test_shell_reconcile_edge_cases.py` names the
row it proves; the register's own summary counts are recomputed from its rows;
and no row is covered only by an assertion-free placeholder. These checks read
the register, the `test_shell*.py` modules and `crates/**/*.rs` as source and
run no shell, so they live in the lint tier (plan_test_speed_tiers.md C-009):
as acceptance targets they had to be handed every `test_shell*` module as a
declared input, or be served a cached verdict over a tree they never read.
"""

from __future__ import annotations

import ast
import re
from collections import Counter
from pathlib import Path

import pytest


def _locate_register() -> Path | None:
    """Find the edge-case register, or ``None`` when this module runs outside the repo.

    The shell-zoo container bind-mounts this file alone at ``/work``, so the
    repo-relative walk has no ancestors to climb and raises ``IndexError`` at
    import time — which took the whole zoo leg down with a collection error
    rather than skipping one repo-consistency check.
    """
    here = Path(__file__).resolve()
    for ancestor in here.parents:
        candidate = ancestor / ".claude" / "artifacts" / "analysis_shell_env_edge_cases.md"
        if candidate.is_file():
            return candidate
    return None


_REGISTER_PATH = _locate_register()


# The acceptance module the register's rows are traced against. These checks
# read it, and their siblings, as source; they left it for the lint tier so
# that no cached acceptance verdict can stand in for a sweep of files it never
# declared as inputs (plan_test_speed_tiers.md C-009).
_THIS_MODULE_PATH = Path(__file__).resolve().parents[1] / "tests" / "test_shell_reconcile_edge_cases.py"


def _split_table_row(line: str) -> list[str]:
    """Split one markdown table row into cells, respecting CommonMark code-span backtick runs and ``\\|`` escapes."""
    s = line.strip()
    s = s.removeprefix("|")
    s = s.removesuffix("|")
    cells: list[str] = []
    buf: list[str] = []
    i = 0
    n = len(s)
    code_run = 0
    while i < n:
        c = s[i]
        if c == "`":
            j = i
            while j < n and s[j] == "`":
                j += 1
            run_len = j - i
            if code_run == 0:
                code_run = run_len
            elif run_len == code_run:
                code_run = 0
            buf.append(s[i:j])
            i = j
            continue
        if c == "\\" and i + 1 < n and s[i + 1] == "|":
            buf.append("|")
            i += 2
            continue
        if c == "|" and code_run == 0:
            cells.append("".join(buf).strip())
            buf = []
            i += 1
            continue
        buf.append(c)
        i += 1
    cells.append("".join(buf).strip())
    return cells


def _parse_register() -> dict[str, dict[str, str]]:
    """Parse every ``EC-*`` row of the register into ``{id: {column: value}}``, skipping the non-primary recap table (``| ID | Gap | Recommendation |``)."""
    rows: dict[str, dict[str, str]] = {}
    header: list[str] | None = None
    for raw_line in _REGISTER_PATH.read_text(encoding="utf-8").split("\n"):
        line = raw_line.rstrip("\n")
        if not line.strip().startswith("|"):
            header = None
            continue
        bare = line.strip().strip("|")
        if re.fullmatch(r"[\s:\-|]+", bare):
            continue
        cells = _split_table_row(line)
        if len(cells) < 2:
            continue
        if cells[0] == "ID":
            header = cells if "Coverage" in cells else None  # only the primary per-row tables carry Coverage
            continue
        if header is None or not re.fullmatch(r"EC-[A-Z]+-\d+", cells[0]):
            continue
        # Structural check the row COUNT cannot substitute for. A row that
        # under-parses still lands in `rows`, so `len(register) == 223` stays
        # green while `Test tier` / `Coverage` hold the wrong cell or none at
        # all — and every gate below then classifies the row as "not mine" and
        # skips it. The usual cause is an unbalanced backtick run swallowing
        # the `|` delimiters for the rest of the line.
        assert len(cells) == len(header), (
            f"{cells[0]} split into {len(cells)} cell(s) against a {len(header)}-column header "
            f"— it would have reached only {header[: len(cells)]}. A mis-split row is invisible "
            "to every traceability gate in this module while the row count still looks right. "
            "Check the row for a code span containing a literal backtick (use a ``…`` span) "
            "or an unclosed one."
        )
        rows[cells[0]] = dict(zip(header, cells, strict=True))
    return rows


_TIER_LABELS = ("rust-unit", "pytest-hostshell", "pytest-shellzoo", "manual-only")


def _primary_tier(tier: str) -> str | None:
    """The earliest-mentioned label in a (possibly combined) ``Test tier`` cell.

    The column's own docstring ("How to read a row") already names this rule:
    "Two tiers named with `+` … and the first is primary." This generalises it
    past the `+`-only case to any free-text ordering (some rows separate a
    second tier with `;` instead), and it is what keeps a cross-reference to
    another row's tier — `` `pytest-hostshell` (EC-FP-001) `` trailing inside an
    otherwise `manual-only` cell — from being misread as this row's own tier:
    a row's own tier is always named first, a citation of someone else's
    always trails as elaboration.
    """
    best_label: str | None = None
    best_index: int | None = None
    for label in _TIER_LABELS:
        index = tier.find(label)
        if index != -1 and (best_index is None or index < best_index):
            best_index = index
            best_label = label
    return best_label


def _this_modules_test_to_ids() -> dict[str, list[str]]:
    """AST-parse THIS file (not the register) for every ``test_*`` function's docstring-cited ``EC-*`` IDs."""
    tree = ast.parse(_THIS_MODULE_PATH.read_text(encoding="utf-8"))
    mapping: dict[str, list[str]] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name.startswith("test_"):
            doc = ast.get_docstring(node) or ""
            ids = re.findall(r"EC-[A-Z]+-\d+", doc.split("\n", 1)[0]) or re.findall(r"EC-[A-Z]+-\d+", doc)
            mapping[node.name] = sorted(set(ids))
    return mapping


def _placeholder_tests() -> set[str]:
    """``<file>::<name>`` for every ``test_*`` in THIS module whose body runs no ``assert`` and no ``pytest.fail``.

    Such a body is a placeholder, not coverage. It reports the same green
    whether the behaviour holds or not — which is the state a test that never
    ran is in (``quality-core.md`` §"Unchecked Green"). The gate below refuses
    to accept one as a row's only coverage.
    """
    tree = ast.parse(_THIS_MODULE_PATH.read_text(encoding="utf-8"))
    placeholders: set[str] = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.FunctionDef) or not node.name.startswith("test_"):
            continue
        kids = list(ast.walk(node))
        if any(isinstance(kid, ast.Assert) for kid in kids):
            continue
        if any(
            isinstance(kid, ast.Call) and isinstance(kid.func, ast.Attribute) and kid.func.attr == "fail"
            for kid in kids
        ):
            continue
        placeholders.add(f"{_THIS_MODULE_PATH.name}::{node.name}")
    return placeholders


# Rows whose Coverage cell declares the register's own ``uncovered`` vocabulary,
# pinned so a tenth row cannot join them quietly. The pin and its history live
# beside the register's tests in the acceptance module, read from there so the
# set has one home.
_UNCOVERED_ROWS = frozenset(
    element.value
    for node in ast.parse(_THIS_MODULE_PATH.read_text(encoding="utf-8")).body
    if isinstance(node, ast.Assign)
    and any(isinstance(target, ast.Name) and target.id == "_UNCOVERED_ROWS" for target in node.targets)
    for element in node.value.args[0].elts
)


# Traceability tests + manual-procedure functions are exempt from "must trace
# to a row" — they ARE the trace, not a covered behaviour.
_TRACEABILITY_EXEMPT_NAMES = frozenset(
    {
        "test_traceability_every_pytest_and_manual_row_names_a_real_covering_test",
        "test_traceability_every_test_in_this_module_traces_to_a_register_row",
        "test_traceability_every_register_row_is_cited_by_a_real_test",
        "test_traceability_the_summary_counts_match_the_register",
        "test_traceability_no_row_is_covered_only_by_an_assertion_free_placeholder",
    }
)


# Where a citing test may live: the acceptance shell modules, and this lint
# module, whose traceability tests are themselves the recorded coverage of the
# rows they were written for (EC-HOOK-015, EC-HOOK-016, EC-PROC-015).
_SHELL_MODULE_GLOBS = ("test/tests/test_shell*.py", "test/lint/test_shell*.py")


def _tests_citing_each_row() -> dict[str, set[str]]:
    """Map every ``EC-*`` id to the tests that cite it, across both harnesses.

    A row is *traceable* when some test names it — in the function name, or in
    the doc comment directly above it. That is the whole mechanism: cheap to
    run, impossible to satisfy by accident, and it fails loudly the moment a
    row is added without a test or a test citing it is deleted.
    """
    root = _REGISTER_PATH.parents[2] if _REGISTER_PATH else None
    citing: dict[str, set[str]] = {}
    if root is None:
        return citing

    for path in sorted(p for pattern in _SHELL_MODULE_GLOBS for p in root.glob(pattern)):
        source = path.read_text(encoding="utf-8")
        lines = source.splitlines()
        for node in ast.walk(ast.parse(source)):
            if not isinstance(node, ast.FunctionDef):
                continue
            if not node.name.startswith(("test_", "manual_procedure_")):
                continue
            # The function's OWN source range (decorators included, since a
            # strict-xfail reason is part of the test), never a `^(?=def )`
            # block that runs on to the next top-level def. Same rule the Rust
            # half below already applies, for the same reason: a citation in
            # ordinary prose — a section banner between two functions — is a
            # cross-reference, never coverage.
            first = min([node.lineno, *(d.lineno for d in node.decorator_list)])
            body = "\n".join(lines[first - 1 : node.end_lineno])
            for row in set(re.findall(r"EC-[A-Z]+-\d+", body)):
                citing.setdefault(row, set()).add(f"{path.name}::{node.name}")

    for path in sorted((root / "crates").rglob("*.rs")):
        lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
        for index, line in enumerate(lines):
            named = re.match(r"\s*(?:async )?fn (\w+)\(", line)
            if not named:
                continue
            cursor, header = index - 1, []
            while cursor >= 0 and lines[cursor].lstrip().startswith(("#[", "///", "//")):
                header.append(lines[cursor])
                cursor -= 1
            # Only real test functions count — a citation in ordinary prose or in
            # a production doc comment is a cross-reference, never coverage.
            if not any("#[test]" in entry or "#[tokio::test]" in entry for entry in header):
                continue
            haystack = named.group(1) + " " + " ".join(header)
            for row in set(re.findall(r"EC-[A-Z]+-\d+", haystack)):
                citing.setdefault(row, set()).add(f"{path.name}::{named.group(1)}")

    return citing


def test_traceability_every_register_row_is_cited_by_a_real_test() -> None:
    """Every one of the 223 rows names a test that exists, in either harness — including the ``rust-unit`` rows.

    The sibling checks below only reach rows whose tier names ``pytest``. That
    left the ``rust-unit`` majority with behaviour that was genuinely covered
    and coverage that nobody could mechanically check — the same class of
    problem as an unchecked green, because a claim nobody re-checks is not
    evidence.
    """
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    citing = _tests_citing_each_row()

    uncited = sorted(row for row in register if row not in citing)
    assert not uncited, (
        f"{len(uncited)} register row(s) are cited by no test in either harness. Add the "
        "EC id to the test that proves the row, or write the test at the row's stated tier "
        f"— never attach an id to a test that does not prove it:\n{uncited}"
    )

    # The reverse direction: a coverage cell must not name a test that is gone.
    dangling = []
    for row, fields in register.items():
        for cited in re.findall(r"`[^`]*::(\w+)`", fields.get("Coverage", "")):
            if not any(cited == name.split("::")[-1] for name in citing.get(row, set())):
                dangling.append(f"{row}: coverage names {cited!r}, which no longer cites it")
    assert not dangling, "coverage cells naming a test that does not cite the row:\n" + "\n".join(dangling)


def test_traceability_the_summary_counts_match_the_register() -> None:
    """The register's ``Coverage:`` summary counts are recomputed from the parsed rows.

    The cell markings two paragraphs above have been gated since `_UNCOVERED_ROWS`
    existed, and were correct. The summary paragraph beside them had no gate and
    drifted: it claimed 91 Rust-cited rows while the tree held 90 — wrong before
    any of the work that added this check, and unnoticed for exactly the reason
    this module exists to refuse. A number nothing recomputes is a claim, not a
    fact.

    Widened past the `Coverage:` sentence to the two paragraphs beside it that
    made the same mistake: the opening `**N rows.**` sentence and the "Tier
    distribution" paragraph both stated a plain integer nothing recomputed, and
    both drifted right along with the coverage counts (they all predate the same
    two added rows) while only the `Coverage:` sentence had a gate.

    Deliberately scoped to the counts. The surrounding prose is not validated and
    should not be: gating prose is how a check becomes something people delete.

    Red state: change any one of the six `Coverage:` numbers, the row count, or
    any one of the four tier-distribution numbers in that paragraph.
    """
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    coverage = [row.get("Coverage", "") for row in register.values()]
    total = len(register)
    cited = sum(1 for c in coverage if c.strip())
    asserting = sum(1 for c in coverage if c.strip() and "uncovered" not in c)
    by_pytest = sum(1 for c in coverage if "test/tests/" in c)
    by_rust = sum(1 for c in coverage if "crates/" in c)
    by_both = sum(1 for c in coverage if "test/tests/" in c and "crates/" in c)

    match = re.search(
        r"\*\*Coverage: (\d+) / (\d+) rows cited, (\d+) by a test that asserts\*\*"
        r"[^0-9]+(\d+) by a pytest\s+test in [^,]+, (\d+) by a Rust `#\[test\]` under\s+"
        r"`crates/`, (\d+) rows by both",
        _REGISTER_PATH.read_text(encoding="utf-8"),
    )
    assert match, (
        "the register's `**Coverage: … rows cited …**` summary paragraph is missing or "
        "reworded past this check. Restore the sentence shape or update this gate — do not "
        "delete it: the paragraph drifted for months precisely while nothing read it."
    )
    claimed = tuple(int(g) for g in match.groups())
    computed = (cited, total, asserting, by_pytest, by_rust, by_both)
    assert claimed == computed, (
        "the register's summary counts disagree with the register itself.\n"
        f"  claimed  (cited/total/asserting/pytest/rust/both): {claimed}\n"
        f"  computed (cited/total/asserting/pytest/rust/both): {computed}\n"
        "Recompute from the tree and edit the paragraph; never edit this gate to match it."
    )
    assert by_pytest + by_rust - by_both == total, (
        f"inclusion-exclusion does not close: {by_pytest} + {by_rust} - {by_both} != {total}. "
        "Some row cites neither harness, which the Coverage column forbids."
    )

    register_text = _REGISTER_PATH.read_text(encoding="utf-8")

    rows_match = re.search(r"\*\*(\d+) rows\.\*\*", register_text)
    assert rows_match, (
        "the register's opening `**N rows.**` sentence is missing or reworded past "
        "this check. Restore the sentence shape or update this gate."
    )
    assert int(rows_match.group(1)) == total, (
        f"the opening '**N rows.**' sentence claims {rows_match.group(1)}, the register "
        f"parses to {total}. Recompute from the tree and edit the sentence."
    )

    tier_counts = Counter(_primary_tier(row.get("Test tier", "")) for row in register.values())
    assert None not in tier_counts, (
        "a row's Test tier cell names none of rust-unit/pytest-hostshell/"
        "pytest-shellzoo/manual-only, so it cannot be assigned a primary tier."
    )
    computed_tiers = tuple(tier_counts[label] for label in _TIER_LABELS)
    assert sum(computed_tiers) == total, (
        f"primary-tier counts {computed_tiers} do not sum to {total} rows — every row "
        "must have exactly one primary tier."
    )
    tier_match = re.search(
        r"\*\*Tier distribution\*\* — `rust-unit` (\d+) . `pytest-hostshell` (\d+) .\s*"
        r"`pytest-shellzoo` (\d+) . `manual-only` (\d+)",
        register_text,
    )
    assert tier_match, (
        "the register's `**Tier distribution** — ...` paragraph is missing or reworded "
        "past this check. Restore the sentence shape or update this gate."
    )
    claimed_tiers = tuple(int(g) for g in tier_match.groups())
    assert claimed_tiers == computed_tiers, (
        "the register's tier-distribution counts disagree with the register itself "
        "(each row counted once, by its first-listed tier).\n"
        f"  claimed  (rust-unit/pytest-hostshell/pytest-shellzoo/manual-only): {claimed_tiers}\n"
        f"  computed (rust-unit/pytest-hostshell/pytest-shellzoo/manual-only): {computed_tiers}\n"
        "Recompute from the tree and edit the paragraph; never edit this gate to match it."
    )


def _shell_module_test_names() -> set[str]:
    """Every ``test_*`` name across the shell test modules, not just this one.

    `_tests_citing_each_row` already globs `test_shell*.py`, so a row is allowed
    to be proven by a test in a sibling module — `EC-HOOK-017` is, because the
    per-prompt reconcile matrix lives in `test_shell_reconcile.py`. The coverage
    gate has to resolve names the same way or a legitimate citation reads as
    dangling. This widens only *where a cited test may live*; the opposite
    direction is untouched, and every `test_*` in THIS module must still trace
    back to a register row.
    """
    root = _REGISTER_PATH.parents[2] if _REGISTER_PATH else None
    if root is None:
        return set()
    names: set[str] = set()
    for path in sorted(p for pattern in _SHELL_MODULE_GLOBS for p in root.glob(pattern)):
        for node in ast.walk(ast.parse(path.read_text(encoding="utf-8"))):
            if isinstance(node, ast.FunctionDef) and node.name.startswith("test_"):
                names.add(node.name)
    return names


def test_traceability_every_pytest_and_manual_row_names_a_real_covering_test() -> None:
    """Every register row whose Test tier names ``pytest-hostshell``/``pytest-shellzoo`` (even combined), or is exactly ``manual-only``, must have a Coverage cell naming a real test in one of the shell test modules (manual rows: a real ``manual_procedure_*`` in THIS module)."""
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    # 220 original rows + 3 added by this module for Discovery corrections 12,
    # 14, 15 (plan_shell_env_overhaul.md §5: EC-HOOK-015, EC-HOOK-016, EC-PROC-015),
    # + EC-LIST-011 (ocx#350) and EC-HOOK-017 (ocx#347) + EC-REC-007 (S-022,
    # `shell_integration_installed`) + EC-REC-008 (finding 97, `lock_refusal`)
    # + EC-GRANT-020 and EC-GRANT-021 (the `paths` subtree form)
    # + EC-GRANT-022, EC-GRANT-023 and EC-GRANT-024 (round 2: tilde expansion,
    # the release-enforced canonical-`project_dir` refusal, and the
    # never-matching-entry diagnostic)
    # + EC-GRANT-025 and EC-GRANT-026 (round 3: the per-platform ASCII-case
    # fold, and the single-wildcard rule)
    # + EC-SCOPE-010 and EC-HOOK-018 (the closed depth-1 toolchain tree: the
    # `active/bin` spelling on the session PATH, and the withhold on a
    # dangling `active`) — both rows, so the count below is fully accounted for
    # + EC-HOOK-018 (ocx#397: a project created under an unchanged `$PWD`).
    assert len(register) == 237, f"the register must still parse to exactly 237 rows; got {len(register)}"
    test_to_ids = _this_modules_test_to_ids()
    known_test_names = set(test_to_ids.keys()) | _shell_module_test_names()
    known_manual_procedures = {
        name
        for name in re.findall(r"^def (manual_procedure_\w+)", _THIS_MODULE_PATH.read_text(encoding="utf-8"), re.MULTILINE)
    }

    missing_coverage: list[str] = []
    dangling_coverage: list[str] = []
    for id_, row in register.items():
        tier = row.get("Test tier", "")
        is_manual = tier.strip().startswith("manual-only")
        is_pytest = ("pytest-hostshell" in tier or "pytest-shellzoo" in tier) and not is_manual
        if not (is_manual or is_pytest):
            continue
        coverage = row.get("Coverage", "").strip()
        if not coverage:
            missing_coverage.append(id_)
            continue
        # The cell names `<path>::<test>`; the bare-name form is still accepted.
        cited_names = re.findall(r"`(?:[^`]*::)?([A-Za-z0-9_]+)`", coverage)
        if is_manual:
            # No prose fallback: a cell that merely repeats the words "manual-only"
            # would pass while naming nothing, which is the same evidence value as
            # this check never running. Every manual row names a real function.
            if not (set(cited_names) & known_manual_procedures):
                dangling_coverage.append(f"{id_}: {coverage!r} names no known manual procedure")
            continue
        if not cited_names or not any(name in known_test_names for name in cited_names):
            dangling_coverage.append(f"{id_}: {coverage!r} names no test function that exists in this module")

    assert not missing_coverage, f"rows with an empty Coverage cell: {missing_coverage}"
    assert not dangling_coverage, "rows whose Coverage cell names something that does not exist:\n" + "\n".join(dangling_coverage)


def test_traceability_every_test_in_this_module_traces_to_a_register_row() -> None:
    """Every ``test_*`` function in this module (except the traceability checks themselves) must cite at least one register row ID that actually exists — no test tracing to a phantom ID."""
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    test_to_ids = _this_modules_test_to_ids()

    untraced: list[str] = []
    dangling: list[str] = []
    for name, ids in test_to_ids.items():
        if name in _TRACEABILITY_EXEMPT_NAMES:
            continue
        if not ids:
            untraced.append(name)
            continue
        for id_ in ids:
            if id_ not in register:
                dangling.append(f"{name} -> {id_}")

    assert not untraced, f"tests whose docstring names no EC-* row at all: {untraced}"
    assert not dangling, "tests tracing to an EC-* ID that does not exist in the register:\n" + "\n".join(dangling)


def test_traceability_no_row_is_covered_only_by_an_assertion_free_placeholder() -> None:
    """A row whose every citing test is a branch-free ``pytest.skip`` executes no assertion on any leg — it is uncovered, and the register must say so rather than read as green.

    The sibling gates check that a coverage claim names something that
    *exists*. They cannot tell an existing test that proves the row from one
    whose whole body is ``pytest.skip(...)``: both report the same green, on
    every platform, forever. The register carries the honest vocabulary for
    this already (``uncovered``); this gate makes using it the only way past.
    """
    if _REGISTER_PATH is None:
        pytest.skip(
            "the edge-case register is not reachable from "
            f"{Path(__file__).resolve()} — no ancestor holds "
            ".claude/artifacts/analysis_shell_env_edge_cases.md. This is a "
            "repo-consistency check; it runs on the host leg, not in the "
            "shell-zoo container, which mounts this file alone."
        )
    register = _parse_register()
    citing = _tests_citing_each_row()
    placeholders = _placeholder_tests()
    assert placeholders, (
        "the placeholder detector matched nothing at all — an AST change that quietly stops "
        "matching would make this whole gate green for the wrong reason"
    )

    placeholder_only: list[str] = []
    stale_marker: list[str] = []
    marked: set[str] = set()
    for id_, row in register.items():
        coverage = row["Coverage"]
        proving = citing.get(id_, set()) - placeholders
        if re.search(r"\buncovered\b", coverage, re.IGNORECASE):
            marked.add(id_)
            if proving:
                stale_marker.append(f"{id_}: marked uncovered, yet {sorted(proving)} asserts for it")
        elif citing.get(id_) and not proving:
            placeholder_only.append(
                f"{id_}: every citing test is assertion-free ({sorted(citing[id_])}) — write the test at "
                "the row's stated tier, or mark the Coverage cell 'uncovered (<owner>)'"
            )

    assert not placeholder_only, (
        "rows whose only coverage is a skip placeholder, reported as green:\n" + "\n".join(placeholder_only)
    )
    assert not stale_marker, (
        "rows marked uncovered that are in fact covered — the marker outlived its cause:\n" + "\n".join(stale_marker)
    )
    assert marked == _UNCOVERED_ROWS, (
        "the set of rows the register admits are uncovered drifted from the pinned set. Marking a row "
        "uncovered is a deliberate, reviewable act, not a way to silence this gate:\n"
        f"  newly marked: {sorted(marked - _UNCOVERED_ROWS)}\n"
        f"  no longer marked: {sorted(_UNCOVERED_ROWS - marked)}"
    )
