# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Docs-quality gate over the docs content root: any finding fails, no page is exempt.

The vendored checks `doc_declaration.py` and `page_type.py` run with `sys.executable` over
`CONTENT_ROOT` only, never over `website/tests/`, `website/plugins/` or build output, and the
reference-shape scan joins them. `page_type` and `shape` findings fail for the whole tree;
`doc_declaration` findings fail for every page, and again for pages changed against
`git merge-base origin/main HEAD`.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest
from test_reference_shape import reference_pages, scan

ROOT = Path(__file__).resolve().parents[2]
CONTENT_ROOT = ROOT / "website" / "src" / "content" / "docs"
CHECKS_DIR = ROOT / ".claude" / "rules" / "docs-quality" / "checks"
CHECK_SCRIPTS = ("doc_declaration.py", "page_type.py")
BASE_REF = "origin/main"
MIN_PAGES_READ = 20
MIN_REFERENCE_PAGES_READ = 20  # the shape scanner reads only `doc_type: reference` pages; 24 today


def merge_base(repo: Path) -> str:
    """The commit `git merge-base origin/main HEAD` names in `repo`."""
    try:
        return _git(repo, "merge-base", BASE_REF, "HEAD")
    except subprocess.CalledProcessError as error:
        raise RuntimeError(
            f"`git merge-base {BASE_REF} HEAD` failed in {repo}: ref {BASE_REF} is missing or shares no "
            f"history with HEAD (fetch it: `git fetch origin main`): {error.stderr.strip()}"
        ) from error


def changed_pages(repo: Path, content_root: Path, base: str) -> set[str]:
    """Pages under `content_root` added or modified since `base`, relative to `content_root`."""
    prefix = content_root.resolve().relative_to(repo.resolve()).as_posix()
    # Committed and uncommitted edits against `base`, then files git has not seen yet.
    listings = (
        _git(repo, "diff", "--name-only", "-z", "--diff-filter=AMR", base),
        _git(repo, "ls-files", "-z", "--others", "--exclude-standard"),
    )
    changed = {name for listing in listings for name in listing.split("\0") if name}
    return {
        name[len(prefix) + 1 :]
        for name in changed
        if name.startswith(prefix + "/") and name.endswith((".md", ".mdx"))
    }


def check_command(script: str, content_root: Path) -> list[str]:
    """argv running one vendored check over `content_root`, led by `sys.executable`."""
    return [sys.executable, str(CHECKS_DIR / script), "--root", str(content_root), "--format", "json"]


def run_checks(content_root: Path) -> dict[str, set[str]]:
    """Run `CHECK_SCRIPTS` over `content_root`: `{page: {"<check>:<rule>", ...}}`, a check named for its script stem."""
    findings: dict[str, set[str]] = {}
    for script in CHECK_SCRIPTS:
        done = subprocess.run(check_command(script, content_root), capture_output=True, text=True, check=False)
        if done.returncode not in (0, 1):
            raise RuntimeError(f"{script} exited {done.returncode}: {done.stderr.strip()}")
        for finding in json.loads(done.stdout):
            page = Path(os.path.relpath(finding.get("page") or finding["path"], content_root)).as_posix()
            findings.setdefault(page, set()).add(f"{Path(script).stem}:{finding['rule']}")
    return findings


def shape_findings(content_root: Path) -> dict[str, set[str]]:
    """The reference-shape findings of `scan`, as `{page: {"shape:<rule>", ...}}`."""
    findings: dict[str, set[str]] = {}
    for finding in scan(content_root):
        location, rule = finding.split(" ")
        findings.setdefault(location.rsplit(":", 1)[0], set()).add(f"shape:{rule}")
    return findings


def gate_findings(content_root: Path) -> dict[str, set[str]]:
    """Every finding that fails the tree: `page_type` and `shape`, never `doc_declaration`."""
    findings = {
        page: {entry for entry in entries if not entry.startswith("doc_declaration:")}
        for page, entries in run_checks(content_root).items()
    }
    for page, entries in shape_findings(content_root).items():
        findings.setdefault(page, set()).update(entries)
    return {page: entries for page, entries in findings.items() if entries}


def _git(repo: Path, *args: str) -> str:
    cmd = ["git", "-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", *args]
    return subprocess.run(cmd, cwd=repo, check=True, capture_output=True, text=True).stdout.strip()


def test_content_root_is_the_docs_content_directory() -> None:
    assert CONTENT_ROOT == ROOT / "website" / "src" / "content" / "docs"
    assert CONTENT_ROOT.is_dir()


def test_checks_run_with_this_interpreter_over_the_content_root() -> None:
    for script in CHECK_SCRIPTS:
        argv = check_command(script, CONTENT_ROOT)
        assert argv[0] == sys.executable
        assert argv[1] == str(CHECKS_DIR / script)
        assert argv[argv.index("--root") + 1] == str(CONTENT_ROOT)


def test_scope_is_pages_changed_since_the_merge_base(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    docs = repo / "docs"
    docs.mkdir(parents=True)
    (docs / "a.md").write_text("a\n")
    (docs / "b.md").write_text("b\n")
    (repo / "outside.md").write_text("outside\n")
    _git(repo, "init", "-b", "main")
    _git(repo, "add", ".")
    _git(repo, "commit", "-m", "base")
    base = _git(repo, "rev-parse", "HEAD")
    _git(repo, "update-ref", "refs/remotes/origin/main", base)
    _git(repo, "checkout", "-b", "feature")
    (docs / "a.md").write_text("a changed\n")
    (docs / "c.md").write_text("c\n")
    (repo / "outside.md").write_text("outside changed\n")
    _git(repo, "add", ".")
    _git(repo, "commit", "-m", "change")

    assert merge_base(repo) == base
    assert changed_pages(repo, docs, base) == {"a.md", "c.md"}


def test_the_docs_tree_holds_no_finding() -> None:
    pages = [p for p in CONTENT_ROOT.rglob("*") if p.suffix in (".md", ".mdx")]
    assert len(pages) >= MIN_PAGES_READ, f"only {len(pages)} pages under {CONTENT_ROOT}"
    references = reference_pages(CONTENT_ROOT)
    assert len(references) >= MIN_REFERENCE_PAGES_READ, f"only {len(references)} reference pages under {CONTENT_ROOT}"
    findings = gate_findings(CONTENT_ROOT)
    assert findings == {}, f"docs-quality findings: {findings}"


def test_every_page_declares_its_type() -> None:
    undeclared = sorted(
        page
        for page, entries in run_checks(CONTENT_ROOT).items()
        if any(entry.startswith("doc_declaration:") for entry in entries)
    )
    assert undeclared == [], f"pages without a valid doc_type declaration: {undeclared}"


def test_merge_base_names_the_missing_ref(tmp_path: Path) -> None:
    _git(tmp_path, "init", "-b", "main")
    _git(tmp_path, "commit", "--allow-empty", "-m", "base")
    with pytest.raises(RuntimeError, match=BASE_REF):
        merge_base(tmp_path)


def test_run_checks_names_each_finding_by_check_and_rule(tmp_path: Path) -> None:
    (tmp_path / "bare.md").write_text("# Bare\n")
    (tmp_path / "sub").mkdir()
    (tmp_path / "sub" / "typed.md").write_text("<!-- doc_type: reference -->\n\n# Typed\n")
    assert run_checks(tmp_path) == {"bare.md": {"doc_declaration:DOC-TYPE-01"}}


def test_changed_pages_declare_their_type() -> None:
    base = merge_base(ROOT)
    undeclared = sorted(
        page
        for page, entries in run_checks(CONTENT_ROOT).items()
        if page in changed_pages(ROOT, CONTENT_ROOT, base)
        and any(entry.startswith("doc_declaration:") for entry in entries)
    )
    assert undeclared == [], f"changed pages without a valid doc_type declaration: {undeclared}"
