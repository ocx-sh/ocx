"""A test carries `requires_testing_build` if and only if it reaches a compiled `__OCX_TESTING_*` seam.

Release bytes lack `__testing`, so `OCX_TEST_RELEASE_BINARY=1` skips marked tests and the readiness run
counts them against `test/limits/<os>-release/`: a missed marker fails on release bytes, a needless
one hides coverage. A seam is a variable shipped Rust code reads, declared `Testing` in
`crates/ocx_env/src/vars.rs`; negative-test strings, harness-read variables and `HARNESS_ONLY` names
are not. A test reaches one through a string literal naming it (docstrings and comments aside) in
itself, a fixture it requests, a helper or constant it uses (imports followed), or code its file runs on
import. A module-level mark covers its module; a doc script is marked in `SEAM_SCRIPTS` (`tests/test_doc_scripts.py`).
"""

from __future__ import annotations

import ast
import collections
import functools
import re
import textwrap
import warnings
from collections.abc import Iterator
from pathlib import Path
from typing import NamedTuple

TEST_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = TEST_ROOT.parent
REGISTRY = Path("crates/ocx_env/src/vars.rs")
SEAM = re.compile(r"__OCX_TESTING_[A-Z_]+")
MARKER = "requires_testing_build"
DOC_RUNNER = Path("tests/test_doc_scripts.py")

# Registered `Testing` variables no shipped Rust code reads: cargo's live-shell tests and the Python
# shell matrix (`src/shell_matrix.py`) read this one, so a test naming it needs no `__testing` build.
HARNESS_ONLY = {"__OCX_TESTING_REQUIRE_LIVE_SHELLS"}

_DECLARED = re.compile(r"\bpub\s+(__OCX_TESTING_[A-Z_]+)\s*:")
_DECLARED_TESTING = re.compile(r"\bpub\s+(__OCX_TESTING_[A-Z_]+)\s*:[^;]*,\s*Testing\s*;")
# Clippy's `items_after_test_module` keeps non-test items above the first top-level test cfg.
_TEST_CFG = re.compile(r"^#\[cfg\((?:all\()?test\b", re.MULTILINE)


def registered_seams(repo: Path) -> tuple[set[str], set[str]]:
    """`(every __OCX_TESTING_* the registry declares, those declared with the `Testing` visibility)`."""
    text = (repo / REGISTRY).read_text(encoding="utf-8")
    return set(_DECLARED.findall(text)), set(_DECLARED_TESTING.findall(text))


def _is_test_source(repo: Path, path: Path) -> bool:
    return any(
        part == "tests" or part.startswith(("tests_", "tests.")) or part.endswith("_tests.rs")
        for part in path.relative_to(repo).parts
    )


def shipped_seams(repo: Path, registered: set[str]) -> tuple[set[str], int]:
    """The registered seams that Rust code outside `#[cfg(test)]` names, and how many Rust files were read."""
    found: set[str] = set()
    read = 0
    for path in sorted((repo / "crates").glob("*/src/**/*.rs")):
        if path == repo / REGISTRY or _is_test_source(repo, path):
            continue
        text = path.read_text(encoding="utf-8")
        cut = _TEST_CFG.search(text)
        shipped = text[: cut.start()] if cut else text
        found.update(name for name in registered if name in shipped)
        read += 1
    return found, read


def _module_names(root: Path, path: Path) -> set[str]:
    """Dotted names `path` is importable under: `tests.x`, `src.x`, and the bare name `pythonpath` allows."""
    parts = list(path.relative_to(root).with_suffix("").parts)
    if parts[-1] == "__init__":
        parts = parts[:-1]
    names = {".".join(parts)}
    if parts[0] == "src" and len(parts) > 1:
        names.add(".".join(parts[1:]))
    if parts[0] == "tests" and len(parts) == 2:
        names.add(parts[1])
    return names


def _bindings(tree: ast.Module, by_name: dict[str, Path]) -> dict[str, list[tuple[Path, str | None]]]:
    """Local name -> `(file, symbol)` it imports, or `(file, None)` for a whole module, from imports at any depth."""
    found: dict[str, list[tuple[Path, str | None]]] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for a in node.names:
                if a.name in by_name:
                    found.setdefault(a.asname or a.name.split(".")[0], []).append((by_name[a.name], None))
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            for a in node.names:
                local = a.asname or a.name
                if f"{node.module}.{a.name}" in by_name:
                    found.setdefault(local, []).append((by_name[f"{node.module}.{a.name}"], None))
                if node.module in by_name:
                    found.setdefault(local, []).append((by_name[node.module], a.name))
    return found


def _seam_nodes(tree: ast.Module, seams: set[str]) -> list[ast.Constant]:
    """The string literals, docstrings aside, that name one of `seams`."""
    docstrings = {
        id(node.body[0].value)
        for node in ast.walk(tree)
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
        and node.body
        and isinstance(node.body[0], ast.Expr)
        and isinstance(node.body[0].value, ast.Constant)
    }
    return [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Constant)
        and isinstance(node.value, str)
        and id(node) not in docstrings
        and seams & set(SEAM.findall(node.value))
    ]


def _carries_marker(tree: ast.Module) -> bool:
    return any(
        isinstance(node, ast.Assign)
        and any(isinstance(t, ast.Name) and t.id == "pytestmark" for t in node.targets)
        and MARKER in ast.unparse(node.value)
        for node in tree.body
    )


def _names_marker(tree: ast.Module) -> bool:
    return any(isinstance(node, ast.Attribute) and node.attr == MARKER for node in ast.walk(tree))


def _decorated(node: ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef) -> bool:
    """A bare `@pytest.mark.requires_testing_build`; one inside `pytest.param(marks=...)` marks a single case."""
    return any(isinstance(d, ast.Attribute) and d.attr == MARKER for d in node.decorator_list)


_DEFS = (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)
_BRANCHES = (ast.If, ast.Try, ast.TryStar, ast.With, ast.For, ast.While)
# Calls whose string arguments name fixtures: `usefixtures("a")`, `getfixturevalue("a")`, `parametrize("a", ..., indirect=True)`.
_FIXTURE_CALLS = {"usefixtures", "getfixturevalue", "parametrize"}
_MODULE = "<module>"
_Key = tuple[Path, str]


def _statements(body: list[ast.stmt]) -> Iterator[ast.stmt]:
    """Module statements, entering the compound ones that only choose between definitions."""
    for stmt in body:
        if isinstance(stmt, _BRANCHES):
            for field in ("body", "orelse", "finalbody"):
                yield from _statements(getattr(stmt, field, []))
            for handler in getattr(stmt, "handlers", []):
                yield from _statements(handler.body)
        else:
            yield stmt


def _bound(stmt: ast.stmt) -> list[str]:
    """Names a plain assignment binds; empty when a target is not a name (`X["a"] = 1` runs on import)."""
    targets = stmt.targets if isinstance(stmt, ast.Assign) else [stmt.target]
    names = [e for t in targets for e in (t.elts if isinstance(t, ast.Tuple) else [t])]
    return [n.id for n in names] if all(isinstance(n, ast.Name) for n in names) else []


def _symbols(tree: ast.Module) -> tuple[dict[str, list[ast.stmt]], list[ast.stmt]]:
    """Top-level names -> their defining statements, and the statements that run on import.

    `pytestmark` and the `pytest_*` hooks shape every test of the file, so they run on import too.
    """
    defs: dict[str, list[ast.stmt]] = {}
    onload: list[ast.stmt] = []
    for stmt in _statements(tree.body):
        if isinstance(stmt, _DEFS) and not stmt.name.startswith("pytest_"):
            defs.setdefault(stmt.name, []).append(stmt)
        elif isinstance(stmt, (ast.Assign, ast.AnnAssign, ast.AugAssign)) and (names := _bound(stmt)) and "pytestmark" not in names:
            for name in names:
                defs.setdefault(name, []).append(stmt)
        elif not isinstance(stmt, (ast.Import, ast.ImportFrom)):
            onload.append(stmt)
    return defs, onload


def _refs(
    nodes: list[ast.AST], modules: dict[str, list[Path]], seam_ids: set[int]
) -> tuple[set[str], set[tuple[Path, str]], bool]:
    """Names, `module.attr` pairs and whether a seam literal appears in `nodes`.

    A module alias read as `alias.attr` names `attr` alone; a bare alias stands for the whole module.
    """
    names: set[str] = set()
    attrs: set[tuple[Path, str]] = set()
    own = False
    for root in nodes:
        walked = list(ast.walk(root))
        consumed: set[int] = set()
        for n in walked:
            if isinstance(n, ast.Attribute) and isinstance(n.value, ast.Name) and n.value.id in modules:
                attrs.update((m, n.attr) for m in modules[n.value.id])
                consumed.add(id(n.value))
        for n in walked:
            if id(n) in seam_ids:
                own = True
            elif isinstance(n, ast.Name) and id(n) not in consumed:
                names.add(n.id)
            elif isinstance(n, ast.arg):
                names.add(n.arg)
            elif isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute) and n.func.attr in _FIXTURE_CALLS:
                for arg in n.args:
                    if isinstance(arg, ast.Constant) and isinstance(arg.value, str):
                        names.update(part.strip() for part in arg.value.split(","))
    return names, attrs, own


def _fixture(stmt: ast.stmt) -> tuple[str, bool] | None:
    """`(fixture name, autouse)` of a `@pytest.fixture` function, else None."""
    if not isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return None
    for d in stmt.decorator_list:
        if "fixture" not in ast.unparse(d):
            continue
        kwargs = {k.arg: k.value for k in d.keywords} if isinstance(d, ast.Call) else {}
        name = kwargs.get("name")
        autouse = kwargs.get("autouse")
        return (
            name.value if isinstance(name, ast.Constant) else stmt.name,
            isinstance(autouse, ast.Constant) and autouse.value is True,
        )
    return None


def _shared(cls: ast.ClassDef) -> list[ast.AST]:
    """A test class without its tests: decorators, bases and the members every test of it may use."""
    return [
        *cls.decorator_list,
        *cls.bases,
        *(m for m in cls.body if not (isinstance(m, (ast.FunctionDef, ast.AsyncFunctionDef)) and m.name.startswith("test"))),
    ]


class SeamTest(NamedTuple):
    module: Path
    name: str
    reaches: bool  # a seam literal is reachable from the test, its fixtures, its helpers or the code its file runs on import
    marked: bool
    exempt: bool  # marked by a module-level mark on a file that imports a seam-holding file, which needs no proof


def _tests(
    root: Path,
    trees: dict[Path, ast.Module],
    binds: dict[Path, dict[str, list[tuple[Path, str | None]]]],
    seam_ids: set[int],
    users: set[Path],
) -> list[SeamTest]:
    """Every `tests/test_*.py` test, with whether it reaches a seam.

    A test reaches a seam when a name it uses (a parameter naming a fixture included) leads, through
    top-level definitions, imports and conftest fixtures, to a seam literal. Anything unresolved is
    assumed to reach none, so a name that could mean two things counts for both: it can only over-mark.
    Not followed: `import a.b` attribute chains, `getattr` or `globals()` dispatch, inherited test classes,
    seam names built by `+` or written as bytes, and conftest import-time code.
    """
    symbols = {p: _symbols(t) for p, t in trees.items()}
    defs = {p: d for p, (d, _) in symbols.items()}
    conftests = [p for p in trees if p.name == "conftest.py" and p.parent in (root, root / "tests")]
    fixtures: dict[str, set[_Key]] = {}
    local: dict[Path, dict[str, set[_Key]]] = {}  # a module's own fixtures, by the public name pytest resolves
    autouse: dict[Path, set[_Key]] = {}
    for p, d in defs.items():
        for name, stmts in d.items():
            for stmt in stmts:
                if (found := _fixture(stmt)) is None:
                    continue
                if p in conftests:
                    fixtures.setdefault(found[0], set()).add((p, name))
                local.setdefault(p, {}).setdefault(found[0], set()).add((p, name))
                if found[1]:
                    autouse.setdefault(p, set()).add((p, name))

    def lookup(path: Path, name: str, seen: frozenset[_Key] = frozenset()) -> set[_Key]:
        if (path, name) in seen:
            return set()
        if name in defs[path]:
            return {(path, name)}
        return {
            k
            for target, sym in binds[path].get(name, ())
            for k in (lookup(target, sym, seen | {(path, name)}) if sym else whole(target))
        }

    def whole(path: Path) -> set[_Key]:
        return {(path, n) for n in defs[path]} | {(path, _MODULE)}

    # A conftest that imports a fixture re-exports it: pytest registers it under the imported name.
    for c in conftests:
        for bound, targets in binds[c].items():
            for k in (k for t, sym in targets if sym for k in lookup(t, sym)):
                for stmt in defs[k[0]].get(k[1], ()):
                    if (found := _fixture(stmt)) is not None:
                        fixtures.setdefault(bound, set()).add(k)
                        fixtures.setdefault(found[0], set()).add(k)

    edges: dict[_Key, set[_Key]] = {}
    tainted: set[_Key] = set()
    for p in trees:
        modules = {n: [t for t, s in bs if s is None] for n, bs in binds[p].items()}
        modules = {n: ts for n, ts in modules.items() if ts}
        members: dict[str, list[ast.AST]] = {}
        for name, stmts in defs[p].items():
            for stmt in stmts:
                if isinstance(stmt, ast.ClassDef) and name.startswith("Test"):
                    members.setdefault(name, []).extend(_shared(stmt))
                    for m in stmt.body:
                        if isinstance(m, (ast.FunctionDef, ast.AsyncFunctionDef)) and m.name.startswith("test"):
                            members[f"{name}.{m.name}"] = [m]
                            edges[(p, f"{name}.{m.name}")] = {(p, name)}
                else:
                    members.setdefault(name, []).append(stmt)
        members[_MODULE] = list(symbols[p][1])
        edges[(p, _MODULE)] = {(t, _MODULE) for bs in binds[p].values() for t, _ in bs}
        for name, nodes in members.items():
            names, attrs, own = _refs(nodes, modules, seam_ids)
            key = (p, name)
            own_fixtures = local.get(p, {})
            edges.setdefault(key, set()).update(
                k for n in names for k in lookup(p, n) | fixtures.get(n, set()) | own_fixtures.get(n, set())
            )
            edges[key].update(k for t, a in attrs for k in lookup(t, a))
            if own:
                tainted.add(key)
    grew = True
    while grew:
        grew = False
        for key, deps in edges.items():
            if key not in tainted and deps & tainted:
                tainted.add(key)
                grew = True

    found: list[SeamTest] = []
    for p in sorted(trees):
        if not (p.name.startswith("test_") and p.parent == root / "tests") or p == root / DOC_RUNNER:
            continue
        module_marked = _carries_marker(trees[p])
        importer = any(t in users for bs in binds[p].values() for t, _ in bs)
        common = {(p, _MODULE), *autouse.get(p, ()), *(k for c in conftests for k in autouse.get(c, ()))}
        for name, stmts in defs[p].items():
            for stmt in stmts:
                if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef)) and name.startswith("test"):
                    cases = [(name, {(p, name)}, _decorated(stmt))]
                elif isinstance(stmt, ast.ClassDef) and name.startswith("Test"):
                    cases = [
                        (f"{name}.{m.name}", {(p, f"{name}.{m.name}")}, _decorated(stmt) or _decorated(m))
                        for m in stmt.body
                        if isinstance(m, (ast.FunctionDef, ast.AsyncFunctionDef)) and m.name.startswith("test")
                    ]
                else:
                    continue
                for label, deps, decorated in cases:
                    found.append(
                        SeamTest(
                            p,
                            label,
                            bool((deps | common) & tainted),
                            module_marked or decorated,
                            module_marked and not decorated and importer,
                        )
                    )
    return found


@functools.cache
def seam_modules(root: Path, seams: frozenset[str]) -> tuple[list[SeamTest], dict[Path, bool], int]:
    """Classify the `tests/` and `src/` Python files against `seams`.

    Returns `(every test, the seam-free modules and whether each names the marker, files read)`. The
    file count lets a caller refuse a reader that read nothing.
    """
    paths = [p for base in ("tests", "src") for p in (root / base).rglob("*.py")]
    if (root / "conftest.py").exists():
        paths.append(root / "conftest.py")
    trees = {p: ast.parse(p.read_text(encoding="utf-8")) for p in paths}
    by_name = {name: p for p in trees for name in _module_names(root, p)}
    binds = {p: _bindings(tree, by_name) for p, tree in trees.items()}
    seam_ids = {id(n) for tree in trees.values() for n in _seam_nodes(tree, seams)}
    users = {p for p, tree in trees.items() if _seam_nodes(tree, seams)}
    grew = True
    while grew:
        grew = False
        for p, bs in binds.items():
            if p not in users and any(t in users for deps in bs.values() for t, _ in deps):
                users.add(p)
                grew = True
    modules = [p for p in sorted(trees) if p.name.startswith("test_") and p.parent == root / "tests"]
    unneeded = {p: _names_marker(trees[p]) for p in modules if p not in users and p != root / DOC_RUNNER}
    return _tests(root, trees, binds, seam_ids, users), unneeded, len(trees)


def unmarked(root: Path, seams: frozenset[str]) -> list[str]:
    tests, _, _ = seam_modules(root, seams)
    return [f"{t.module.relative_to(root)}::{t.name}" for t in tests if t.reaches and not t.marked]


def over_marked(root: Path, seams: frozenset[str]) -> list[str]:
    """Tests marked though they reach no seam, which release bytes skip for nothing."""
    tests, _, _ = seam_modules(root, seams)
    return [f"{t.module.relative_to(root)}::{t.name}" for t in tests if t.marked and not t.reaches and not t.exempt]


def needlessly_marked(root: Path, seams: frozenset[str]) -> list[str]:
    _, unneeded, _ = seam_modules(root, seams)
    return [str(p.relative_to(root)) for p, marked in unneeded.items() if marked]


def doc_script_seam_users(root: Path, seams: frozenset[str]) -> tuple[set[str], int]:
    """Doc scripts whose code (comment lines aside) names a seam, and how many scripts were read."""
    scripts = sorted((root / "doc_scripts").glob("*.sh"))
    users = {
        p.name
        for p in scripts
        if any(
            seams & set(SEAM.findall(line))
            for line in p.read_text(encoding="utf-8").splitlines()
            if not line.lstrip().startswith("#")
        )
    }
    return users, len(scripts)


def marked_doc_scripts(root: Path) -> set[str]:
    """The names `tests/test_doc_scripts.py` lists in `SEAM_SCRIPTS`, which it marks one by one."""
    tree = ast.parse((root / DOC_RUNNER).read_text(encoding="utf-8"))
    for node in tree.body:
        targets = node.targets if isinstance(node, ast.Assign) else [node.target] if isinstance(node, ast.AnnAssign) else []
        if any(isinstance(t, ast.Name) and t.id == "SEAM_SCRIPTS" for t in targets) and node.value is not None:
            return set(ast.literal_eval(node.value))
    raise AssertionError(f"{DOC_RUNNER} defines no SEAM_SCRIPTS set")


def test_the_rust_registry_and_its_readers_hold():
    registered, testing = registered_seams(REPO_ROOT)
    assert registered == testing, f"declared with a visibility other than `Testing`: {sorted(registered - testing)}"
    assert len(registered) >= 15, f"only {len(registered)} registered seam(s) read — the registry reader lost its subject"
    shipped, read = shipped_seams(REPO_ROOT, registered)
    assert read > 300, f"only {read} Rust file(s) read — the shipped-code scan lost its subject"
    assert registered - shipped == HARNESS_ONLY, (
        f"registered seams no shipped Rust code reads: {sorted(registered - shipped)}, expected {sorted(HARNESS_ONLY)} — "
        "a new test-only switch joins HARNESS_ONLY, a seam the binary reads must show in non-test Rust"
    )


@functools.cache
def _seams() -> frozenset[str]:
    registered, _ = registered_seams(REPO_ROOT)
    return frozenset(shipped_seams(REPO_ROOT, registered)[0])


def test_every_test_reaching_a_seam_carries_the_marker():
    tests, _, read = seam_modules(TEST_ROOT, _seams())
    assert read > 100, f"only {read} file(s) read — the scan lost its subject"
    assert len(tests) > 1500, f"only {len(tests)} test(s) found — the scan lost its subject"
    reaching = sum(t.reaches for t in tests)
    assert reaching > 200, f"only {reaching} test(s) reach a seam — the scan lost its subject"
    missing = unmarked(TEST_ROOT, _seams())
    assert not missing, f"reach a compiled `__OCX_TESTING_*` seam without `{MARKER}` on the test or a module-level `pytestmark`: {missing}"


def test_no_module_carries_the_marker_without_a_seam():
    _, unneeded, _ = seam_modules(TEST_ROOT, _seams())
    assert len(unneeded) > 50, f"only {len(unneeded)} seam-free module(s) found — the scan lost its subject"
    extra = needlessly_marked(TEST_ROOT, _seams())
    assert not extra, f"carry `{MARKER}` but name no compiled `__OCX_TESTING_*` seam, so release bytes skip them for nothing: {extra}"


def test_over_marked_tests_are_reported():
    """Not a failure: a mark a test does not need hides coverage on release bytes, so it is listed for a cleanup."""
    extra = over_marked(TEST_ROOT, _seams())
    if extra:
        per_module = collections.Counter(name.split("::")[0] for name in extra)
        warnings.warn(f"{len(extra)} test(s) carry `{MARKER}` but reach no seam: {dict(per_module)}", stacklevel=1)


def test_every_doc_script_seam_user_is_marked_by_the_runner():
    users, read = doc_script_seam_users(TEST_ROOT, _seams())
    assert read > 50, f"only {read} doc script(s) read — the scan lost its subject"
    assert users, "no doc script names a seam — the scan lost its subject"
    marked = marked_doc_scripts(TEST_ROOT)
    assert marked == users, (
        f"{DOC_RUNNER} marks {sorted(marked)} but the doc scripts naming a compiled seam are {sorted(users)}: "
        "SEAM_SCRIPTS must list exactly those"
    )


_FIXTURE_SEAMS = frozenset({"__OCX_TESTING_LIBC", "__OCX_TESTING_FAULT"})


def _tree(tmp_path: Path, test_source: str, conftest: str | None = None) -> Path:
    (tmp_path / "src").mkdir()
    (tmp_path / "tests").mkdir()
    (tmp_path / "tests" / "seam_helper.py").write_text('SEAM = "__OCX_TESTING_LIBC"\nOTHER = "plain"\n', encoding="utf-8")
    (tmp_path / "tests" / "test_x.py").write_text(textwrap.dedent(test_source), encoding="utf-8")
    if conftest is not None:
        (tmp_path / "tests" / "conftest.py").write_text(textwrap.dedent(conftest), encoding="utf-8")
    return tmp_path


_MARK = "@pytest.mark.requires_testing_build\n"


def test_unmarked_test_using_a_seam_helper_is_red(tmp_path: Path):
    root = _tree(tmp_path, "import pytest\nfrom seam_helper import SEAM\ndef test_a():\n    assert SEAM\n")
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_unmarked_direct_use_is_red(tmp_path: Path):
    root = _tree(tmp_path, 'import pytest\ndef test_a():\n    env = "__OCX_TESTING_FAULT"\n')
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_module_mark_covers_every_test_that_reaches_a_seam(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\nfrom seam_helper import SEAM\npytestmark = pytest.mark.requires_testing_build\ndef test_a():\n    assert SEAM\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_marker_merged_into_a_list_is_green(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\nfrom seam_helper import SEAM\n"
        "pytestmark = [pytest.mark.smoke, pytest.mark.requires_testing_build]\ndef test_a():\n    assert SEAM\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_seam_inside_a_marked_test_is_green(tmp_path: Path):
    root = _tree(
        tmp_path,
        f'import pytest\n{_MARK}def test_a():\n    env = "__OCX_TESTING_FAULT"\n\ndef test_b():\n    pass\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_seam_in_an_unmarked_test_beside_a_marked_one_is_red(tmp_path: Path):
    root = _tree(
        tmp_path,
        f'import pytest\n{_MARK}def test_a():\n    pass\n\ndef test_b():\n    env = "__OCX_TESTING_FAULT"\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_b"]


def test_marked_test_does_not_cover_a_sibling_using_the_same_helper(tmp_path: Path):
    root = _tree(
        tmp_path,
        f"import pytest\nfrom seam_helper import SEAM\n{_MARK}def test_a():\n    assert SEAM\n\ndef test_b():\n    assert SEAM\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_b"]


def test_a_mark_on_one_parametrized_case_does_not_mark_the_test(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\nfrom seam_helper import SEAM\n"
        '@pytest.mark.parametrize("x", [pytest.param(1, marks=pytest.mark.requires_testing_build), 2])\n'
        "def test_a(x):\n    assert SEAM\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_seam_reached_through_a_renamed_module_fixture_is_red(tmp_path: Path):
    # pytest resolves a parameter by the fixture's `name=`, never by its def name.
    root = _tree(
        tmp_path,
        'import pytest\n@pytest.fixture(name="env")\ndef _env():\n    return "__OCX_TESTING_FAULT"\n\n'
        "def test_a(env):\n    pass\n\ndef test_b():\n    pass\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_seam_reached_through_a_module_helper_is_red(tmp_path: Path):
    root = _tree(
        tmp_path,
        'import pytest\ndef test_a():\n    _helper()\n\ndef test_b():\n    pass\n\ndef _helper():\n    return "__OCX_TESTING_FAULT"\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_seam_reached_through_a_requested_fixture_is_red(tmp_path: Path):
    root = _tree(
        tmp_path,
        'import pytest\n@pytest.fixture\ndef forged():\n    return "__OCX_TESTING_FAULT"\n\n'
        "def test_a(forged):\n    pass\n\ndef test_b():\n    pass\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_seam_reached_through_a_chain_of_conftest_fixtures_is_red(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\ndef test_a(outer):\n    pass\n\ndef test_b():\n    pass\n",
        conftest=(
            "import pytest\n"
            '@pytest.fixture\ndef inner():\n    return "__OCX_TESTING_FAULT"\n\n'
            "@pytest.fixture\ndef outer(inner):\n    return inner\n"
        ),
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_seam_reached_through_a_fixture_a_conftest_re_exports_is_red(tmp_path: Path):
    # `tests/conftest.py` imports the Sigstore fixtures from `tests/fixtures/`; pytest registers them by import.
    root = _tree(
        tmp_path,
        "import pytest\ndef test_a(stack):\n    pass\n\ndef test_b():\n    pass\n",
        conftest="from tests.fixtures.stack import stack  # noqa: F401\n",
    )
    (root / "tests" / "fixtures").mkdir()
    (root / "tests" / "fixtures" / "stack.py").write_text(
        'import pytest\n@pytest.fixture\ndef stack():\n    return "__OCX_TESTING_FAULT"\n', encoding="utf-8"
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_autouse_fixture_naming_a_seam_reaches_every_test(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\ndef test_a():\n    pass\n\ndef test_b():\n    pass\n",
        conftest='import pytest\n@pytest.fixture(autouse=True)\ndef always():\n    return "__OCX_TESTING_FAULT"\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a", "tests/test_x.py::test_b"]


def test_seam_in_code_that_runs_on_import_reaches_every_test(tmp_path: Path):
    root = _tree(
        tmp_path,
        'import os\nos.environ["__OCX_TESTING_FAULT"] = "1"\ndef test_a():\n    pass\n\ndef test_b():\n    pass\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a", "tests/test_x.py::test_b"]


def test_a_module_attribute_names_only_the_attribute_it_reads(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import seam_helper\ndef test_a():\n    seam_helper.SEAM\n\ndef test_b():\n    seam_helper.OTHER\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_a_seam_constant_no_test_uses_needs_no_marker(tmp_path: Path):
    root = _tree(tmp_path, 'import pytest\nENV = "__OCX_TESTING_FAULT"\ndef test_a():\n    pass\n')
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_a_test_class_is_judged_per_method(tmp_path: Path):
    root = _tree(
        tmp_path,
        'import pytest\nclass TestX:\n    def test_a(self):\n        env = "__OCX_TESTING_FAULT"\n\n    def test_b(self):\n        pass\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::TestX.test_a"]


def test_a_marked_test_class_covers_its_methods(tmp_path: Path):
    root = _tree(
        tmp_path,
        f'import pytest\n{_MARK}class TestX:\n    def test_a(self):\n        env = "__OCX_TESTING_FAULT"\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_module_with_no_seam_needs_no_marker(tmp_path: Path):
    root = _tree(tmp_path, "import pytest\ndef test_a():\n    pass\n")
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_a_name_that_is_not_a_compiled_seam_needs_no_marker(tmp_path: Path):
    root = _tree(
        tmp_path,
        'import pytest\nINVALID = "__OCX_TESTING_X"\nHARNESS = "__OCX_TESTING_REQUIRE_LIVE_SHELLS"\n'
        "def test_a():\n    return INVALID, HARNESS\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_a_comment_or_docstring_naming_a_seam_needs_no_marker(tmp_path: Path):
    root = _tree(
        tmp_path,
        '"""Does not set __OCX_TESTING_LIBC."""\nimport pytest\n# __OCX_TESTING_FAULT is not set here.\n'
        'def test_a():\n    """Nor __OCX_TESTING_LIBC here."""\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_a_marked_test_that_reaches_no_seam_is_reported(tmp_path: Path):
    root = _tree(tmp_path, f"import pytest\n{_MARK}def test_a():\n    pass\n\ndef test_b():\n    pass\n")
    assert over_marked(root, _FIXTURE_SEAMS) == ["tests/test_x.py::test_a"]


def test_a_module_mark_on_an_importer_of_a_seam_file_is_accepted(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\nfrom seam_helper import OTHER\npytestmark = pytest.mark.requires_testing_build\ndef test_a():\n    assert OTHER\n",
    )
    assert over_marked(root, _FIXTURE_SEAMS) == []


def test_marker_without_a_seam_is_red(tmp_path: Path):
    root = _tree(tmp_path, "import pytest\npytestmark = pytest.mark.requires_testing_build\n")
    assert needlessly_marked(root, _FIXTURE_SEAMS) == ["tests/test_x.py"]


def test_marker_with_a_seam_is_not_needless(tmp_path: Path):
    root = _tree(tmp_path, 'import pytest\nENV = "__OCX_TESTING_FAULT"\npytestmark = pytest.mark.requires_testing_build\n')
    assert needlessly_marked(root, _FIXTURE_SEAMS) == []


def test_doc_script_comment_naming_a_seam_is_not_a_use(tmp_path: Path):
    (tmp_path / "doc_scripts").mkdir()
    (tmp_path / "doc_scripts" / "a.sh").write_text("# __OCX_TESTING_LIBC would go here\nocx about\n", encoding="utf-8")
    (tmp_path / "doc_scripts" / "b.sh").write_text('export __OCX_TESTING_LIBC="glibc"\n', encoding="utf-8")
    assert doc_script_seam_users(tmp_path, _FIXTURE_SEAMS) == ({"b.sh"}, 2)


def _repo(tmp_path: Path) -> Path:
    registry = tmp_path / REGISTRY
    registry.parent.mkdir(parents=True)
    registry.write_text(
        "    pub __OCX_TESTING_A: String, Testing;\n"
        "    pub __OCX_TESTING_B: Path, Testing;\n"
        "    pub __OCX_TESTING_C: Bool, Plumbing;\n",
        encoding="utf-8",
    )
    src = tmp_path / "crates" / "seam_fixture" / "src"
    (src / "tests").mkdir(parents=True)
    (src / "lib.rs").write_text(
        "fn read() { ocx_env::__OCX_TESTING_A.get(); }\n#[cfg(test)]\nmod tests { fn f() { ocx_env::__OCX_TESTING_B.get(); } }\n",
        encoding="utf-8",
    )
    (src / "tests" / "b.rs").write_text("fn f() { ocx_env::__OCX_TESTING_B.get(); }\n", encoding="utf-8")
    return tmp_path


def test_only_the_names_declared_testing_are_registered(tmp_path: Path):
    registered, testing = registered_seams(_repo(tmp_path))
    assert registered == {"__OCX_TESTING_A", "__OCX_TESTING_B", "__OCX_TESTING_C"}
    assert testing == {"__OCX_TESTING_A", "__OCX_TESTING_B"}


def test_a_name_only_test_code_reads_is_not_shipped(tmp_path: Path):
    repo = _repo(tmp_path)
    shipped, read = shipped_seams(repo, registered_seams(repo)[1])
    assert shipped == {"__OCX_TESTING_A"}
    assert read == 1
