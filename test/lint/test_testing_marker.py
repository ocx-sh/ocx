"""A module carries `requires_testing_build` if and only if it drives a compiled `__OCX_TESTING_*` seam.

Release bytes lack `__testing`, so `OCX_TEST_RELEASE_BINARY=1` skips marked tests (`test/conftest.py`)
and the readiness run counts them against `test/limits/<os>-release/`: a missed marker fails on
release bytes, a needless one hides coverage. A seam is a variable shipped Rust code reads, declared
`Testing` in `crates/ocx_env/src/vars.rs`; negative-test strings, harness-read variables and
`HARNESS_ONLY` names are not. A user is a file with a string literal naming one (comments and
docstrings do not count) or importing such a file. Marking only the tests that use a seam suffices,
except for an importer; a doc script is marked through `SEAM_SCRIPTS` in `tests/test_doc_scripts.py`.
"""

from __future__ import annotations

import ast
import functools
import re
import textwrap
from pathlib import Path

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


def _imports(tree: ast.Module, by_name: dict[str, Path]) -> set[Path]:
    found: set[Path] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            found.update(by_name[a.name] for a in node.names if a.name in by_name)
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            if node.module in by_name:
                found.add(by_name[node.module])
            found.update(by_name[f"{node.module}.{a.name}"] for a in node.names if f"{node.module}.{a.name}" in by_name)
    return found


def _seam_spans(tree: ast.Module, seams: set[str]) -> list[tuple[int, int]]:
    """Line spans of the string literals, docstrings aside, that name one of `seams`."""
    docstrings = {
        id(node.body[0].value)
        for node in ast.walk(tree)
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
        and node.body
        and isinstance(node.body[0], ast.Expr)
        and isinstance(node.body[0].value, ast.Constant)
    }
    return [
        (node.lineno, node.end_lineno or node.lineno)
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


def _marks_every_use(tree: ast.Module, spans: list[tuple[int, int]]) -> bool:
    """Every literal naming a seam sits inside a top-level test or class decorated with the marker."""
    marked = [
        (min(d.lineno for d in node.decorator_list), node.end_lineno or node.lineno)
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
        and any(MARKER in ast.unparse(d) for d in node.decorator_list)
    ]
    return bool(spans) and all(any(lo <= start and end <= hi for lo, hi in marked) for start, end in spans)


@functools.cache
def seam_modules(root: Path, seams: frozenset[str]) -> tuple[dict[Path, bool], dict[Path, bool], int]:
    """Classify the `tests/` and `src/` Python files against `seams`.

    Returns `(seam users, every other file, files read)`: each maps a `test_*.py` module under
    `root/tests` to whether it is marked as the marker lint requires; the first holds the modules
    that use a seam directly or through an import, the second those that use none and so may not
    carry the marker. The file count lets a caller refuse a reader that read nothing.
    """
    sources = {p: p.read_text(encoding="utf-8") for base in ("tests", "src") for p in (root / base).rglob("*.py")}
    trees = {p: ast.parse(text) for p, text in sources.items()}
    spans = {p: _seam_spans(tree, seams) for p, tree in trees.items()}
    by_name = {name: p for p in sources for name in _module_names(root, p)}
    imports = {p: _imports(tree, by_name) for p, tree in trees.items()}
    users = {p for p, found in spans.items() if found}
    grew = True
    while grew:
        grew = False
        for p, deps in imports.items():
            if p not in users and deps & users:
                users.add(p)
                grew = True
    modules = [p for p in sorted(sources) if p.name.startswith("test_") and p.parent == root / "tests"]
    needed = {
        p: _carries_marker(trees[p]) or (not imports[p] & users and _marks_every_use(trees[p], spans[p]))
        for p in modules
        if p in users
    }
    unneeded = {p: _names_marker(trees[p]) for p in modules if p not in users and p != root / DOC_RUNNER}
    return needed, unneeded, len(sources)


def unmarked(root: Path, seams: frozenset[str]) -> list[str]:
    needed, _, _ = seam_modules(root, seams)
    return [str(p.relative_to(root)) for p, marked in needed.items() if not marked]


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


def test_every_seam_user_carries_the_marker():
    needed, _, read = seam_modules(TEST_ROOT, _seams())
    assert read > 100, f"only {read} file(s) read — the scan lost its subject"
    assert len(needed) > 20, f"only {len(needed)} seam user(s) found — the scan lost its subject"
    missing = unmarked(TEST_ROOT, _seams())
    assert not missing, f"name a compiled `__OCX_TESTING_*` seam without `pytestmark` carrying `{MARKER}`: {missing}"


def test_no_module_carries_the_marker_without_a_seam():
    _, unneeded, _ = seam_modules(TEST_ROOT, _seams())
    assert len(unneeded) > 50, f"only {len(unneeded)} seam-free module(s) found — the scan lost its subject"
    extra = needlessly_marked(TEST_ROOT, _seams())
    assert not extra, f"carry `{MARKER}` but name no compiled `__OCX_TESTING_*` seam, so release bytes skip them for nothing: {extra}"


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


def _tree(tmp_path: Path, test_source: str) -> Path:
    (tmp_path / "src").mkdir()
    (tmp_path / "tests").mkdir()
    (tmp_path / "tests" / "seam_helper.py").write_text('SEAM = "__OCX_TESTING_LIBC"\n', encoding="utf-8")
    (tmp_path / "tests" / "test_x.py").write_text(textwrap.dedent(test_source), encoding="utf-8")
    return tmp_path


def test_unmarked_importer_of_a_seam_helper_is_red(tmp_path: Path):
    root = _tree(tmp_path, "import pytest\nfrom seam_helper import SEAM\n")
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py"]


def test_unmarked_direct_use_is_red(tmp_path: Path):
    root = _tree(tmp_path, 'import pytest\nENV = "__OCX_TESTING_FAULT"\n')
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py"]


def test_marked_importer_is_green(tmp_path: Path):
    root = _tree(tmp_path, "import pytest\nfrom seam_helper import SEAM\npytestmark = pytest.mark.requires_testing_build\n")
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_marker_merged_into_a_list_is_green(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\nfrom seam_helper import SEAM\npytestmark = [pytest.mark.smoke, pytest.mark.requires_testing_build]\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_seam_inside_a_marked_test_is_green(tmp_path: Path):
    root = _tree(
        tmp_path,
        'import pytest\n@pytest.mark.requires_testing_build\ndef test_a():\n    env = "__OCX_TESTING_FAULT"\n\ndef test_b():\n    pass\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_seam_outside_the_marked_test_is_red(tmp_path: Path):
    root = _tree(
        tmp_path,
        'import pytest\nENV = "__OCX_TESTING_FAULT"\n@pytest.mark.requires_testing_build\ndef test_a():\n    pass\n',
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py"]


def test_marked_test_does_not_cover_an_importer(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\nfrom seam_helper import SEAM\n@pytest.mark.requires_testing_build\ndef test_a():\n    assert SEAM\n",
    )
    assert unmarked(root, _FIXTURE_SEAMS) == ["tests/test_x.py"]


def test_module_with_no_seam_needs_no_marker(tmp_path: Path):
    root = _tree(tmp_path, "import pytest\n")
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_a_name_that_is_not_a_compiled_seam_needs_no_marker(tmp_path: Path):
    root = _tree(tmp_path, 'import pytest\nINVALID = "__OCX_TESTING_X"\nHARNESS = "__OCX_TESTING_REQUIRE_LIVE_SHELLS"\n')
    assert unmarked(root, _FIXTURE_SEAMS) == []


def test_a_comment_or_docstring_naming_a_seam_needs_no_marker(tmp_path: Path):
    root = _tree(tmp_path, '"""Does not set __OCX_TESTING_LIBC."""\nimport pytest\n# __OCX_TESTING_FAULT is not set here.\n')
    assert unmarked(root, _FIXTURE_SEAMS) == []


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
    src = tmp_path / "crates" / "ocx_x" / "src"
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
