"""Every acceptance module that drives a `__OCX_TESTING_*` seam carries `requires_testing_build`.

Release bytes lack the `__testing` feature, so `OCX_TEST_RELEASE_BINARY=1` skips the marked
modules by name (`test/conftest.py`) and the readiness run counts those skips against its own
ceiling (`test/limits/<os>-release/`). A module the marker misses would instead fail on release
bytes, or hide inside a green. The set is derived, never listed: a `tests/` or `src/` file whose
text names a `__OCX_TESTING_[A-Z_]+` variable is a seam user, and so is every file importing one.
A module that names a seam only inside tests marked one by one may mark just those, so its other
tests still run on release bytes; an importer always carries the module-level marker.
"""

from __future__ import annotations

import ast
import re
import textwrap
from pathlib import Path

TEST_ROOT = Path(__file__).resolve().parents[1]
SEAM = re.compile(r"__OCX_TESTING_[A-Z_]+")
MARKER = "requires_testing_build"


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


def _carries_marker(tree: ast.Module) -> bool:
    return any(
        isinstance(node, ast.Assign)
        and any(isinstance(t, ast.Name) and t.id == "pytestmark" for t in node.targets)
        and MARKER in ast.unparse(node.value)
        for node in tree.body
    )


def _marks_every_use(tree: ast.Module, text: str) -> bool:
    """Every line naming a seam sits inside a top-level test or class decorated with the marker."""
    spans = [
        (min(d.lineno for d in node.decorator_list), node.end_lineno or node.lineno)
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
        and any(MARKER in ast.unparse(d) for d in node.decorator_list)
    ]
    lines = [n for n, line in enumerate(text.splitlines(), 1) if SEAM.search(line)]
    return bool(lines) and all(any(lo <= n <= hi for lo, hi in spans) for n in lines)


def seam_modules(root: Path) -> tuple[dict[Path, bool], int]:
    """Every `test_*.py` under `root/tests` that uses a seam, directly or through an import: path -> carries the marker.

    Also returns how many files were read, so a caller can refuse a reader that read nothing.
    """
    sources = {p: p.read_text(encoding="utf-8") for base in ("tests", "src") for p in (root / base).rglob("*.py")}
    trees = {p: ast.parse(text) for p, text in sources.items()}
    by_name = {name: p for p in sources for name in _module_names(root, p)}
    imports = {p: _imports(tree, by_name) for p, tree in trees.items()}
    users = {p for p, text in sources.items() if SEAM.search(text)}
    grew = True
    while grew:
        grew = False
        for p, deps in imports.items():
            if p not in users and deps & users:
                users.add(p)
                grew = True
    return {
        p: _carries_marker(trees[p]) or (not imports[p] & users and _marks_every_use(trees[p], sources[p]))
        for p in sorted(users)
        if p.name.startswith("test_") and p.parent == root / "tests"
    }, len(sources)


def unmarked(root: Path) -> list[str]:
    modules, _ = seam_modules(root)
    return [str(p.relative_to(root)) for p, marked in modules.items() if not marked]


def test_every_seam_user_carries_the_marker():
    modules, read = seam_modules(TEST_ROOT)
    assert read > 100, f"only {read} file(s) read — the scan lost its subject"
    assert len(modules) > 20, f"only {len(modules)} seam user(s) found — the scan lost its subject"
    missing = unmarked(TEST_ROOT)
    assert not missing, f"name a `__OCX_TESTING_*` seam without `pytestmark` carrying `{MARKER}`: {missing}"


def _tree(tmp_path: Path, test_source: str) -> Path:
    (tmp_path / "src").mkdir()
    (tmp_path / "tests").mkdir()
    (tmp_path / "tests" / "seam_helper.py").write_text('SEAM = "__OCX_TESTING_LIBC"\n', encoding="utf-8")
    (tmp_path / "tests" / "test_x.py").write_text(textwrap.dedent(test_source), encoding="utf-8")
    return tmp_path


def test_unmarked_importer_of_a_seam_helper_is_red(tmp_path: Path):
    root = _tree(tmp_path, "import pytest\nfrom seam_helper import SEAM\n")
    assert unmarked(root) == ["tests/test_x.py"]


def test_unmarked_direct_use_is_red(tmp_path: Path):
    root = _tree(tmp_path, 'import pytest\nENV = "__OCX_TESTING_FAULT"\n')
    assert unmarked(root) == ["tests/test_x.py"]


def test_marked_importer_is_green(tmp_path: Path):
    root = _tree(tmp_path, "import pytest\nfrom seam_helper import SEAM\npytestmark = pytest.mark.requires_testing_build\n")
    assert unmarked(root) == []


def test_marker_merged_into_a_list_is_green(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\nfrom seam_helper import SEAM\npytestmark = [pytest.mark.smoke, pytest.mark.requires_testing_build]\n",
    )
    assert unmarked(root) == []


def test_seam_inside_a_marked_test_is_green(tmp_path: Path):
    root = _tree(
        tmp_path,
        'import pytest\n@pytest.mark.requires_testing_build\ndef test_a():\n    env = "__OCX_TESTING_FAULT"\n\ndef test_b():\n    pass\n',
    )
    assert unmarked(root) == []


def test_seam_outside_the_marked_test_is_red(tmp_path: Path):
    root = _tree(
        tmp_path,
        'import pytest\nENV = "__OCX_TESTING_FAULT"\n@pytest.mark.requires_testing_build\ndef test_a():\n    pass\n',
    )
    assert unmarked(root) == ["tests/test_x.py"]


def test_marked_test_does_not_cover_an_importer(tmp_path: Path):
    root = _tree(
        tmp_path,
        "import pytest\nfrom seam_helper import SEAM\n@pytest.mark.requires_testing_build\ndef test_a():\n    assert SEAM\n",
    )
    assert unmarked(root) == ["tests/test_x.py"]


def test_module_with_no_seam_needs_no_marker(tmp_path: Path):
    root = _tree(tmp_path, "import pytest\n")
    assert unmarked(root) == []
