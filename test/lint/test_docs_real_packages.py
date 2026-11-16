# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The docs name only packages, tags and version output that exist on ocx.sh.

Scans the docs, README, catalog and doc scripts for package identifiers and compares them
with `src/real_packages.py`. A finding is `"<path>: <identifier>: <reason>"`; those that
predate the table sit in `BASELINE`, which may only shrink. No network, no binary. Rules:
host-qualified, tagged and untagged identifiers, physical repositories, declared state
packages, script tags and version output lines. Each heuristic's limit is a `shortcut:`.
"""

from __future__ import annotations

import re
import subprocess
from collections.abc import Iterator

import pytest

from src.doc_scripts import parse_doc_header
from src.helpers import PROJECT_ROOT, env_key
from src.real_packages import PLACEHOLDER_NAMESPACES, REAL_PACKAGES
from src.state_providers import DECLARED_PACKAGES

# Identifier pieces. A trailing `.`/`-` belongs to the sentence, never to the name.
_NAME = r"[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?"
_TAG = r"[A-Za-z0-9_](?:[A-Za-z0-9._-]*[A-Za-z0-9_])?"
_URL = re.compile(r"https?://\S+")
_OS_NAMESPACES = frozenset({"linux", "darwin", "windows", "wasip1", "wasip2", "any"})
# Paths of the ocx.sh website, not packages.
_SITE_SEGMENTS = frozenset({"docs", "schemas", "integrations"})
_FILE_SUFFIXES = (".rs", ".py", ".md", ".mdx", ".toml", ".json", ".sh", ".yml", ".yaml", ".lock", ".txt", ".ts", ".ps1")

_HOST_IDENT = re.compile(rf"(?<![\w./:@~$-])ocx\.sh/({_NAME})(?:/({_NAME}))?(?::({_TAG}))?")
_TAGGED_IDENT = re.compile(rf"(?<![\w./:@~$<-])([a-z0-9][a-z0-9_-]*)/({_NAME}):({_TAG})(?![A-Za-z0-9._<*-])")
_PHYSICAL_REPO = re.compile(r"oci://ghcr\.io/ocx-contrib/([a-z0-9][a-z0-9/_.-]*[a-z0-9])")
_SCRIPT_TAG = re.compile(rf"\$REPO_([A-Z0-9_]+):({_TAG})")
_FENCE = re.compile(r"^\s*```(\S*)")

_TABLE_NAMESPACES = frozenset(name.split("/", 1)[0] for name in REAL_PACKAGES)
_UNTAGGED_IDENT = re.compile(
    rf"(?<![\w./:@~$<-])({'|'.join(sorted(map(re.escape, _TABLE_NAMESPACES)))})/({_NAME})(?![/\w-]|:[A-Za-z0-9_]|\.[a-z0-9])"
)
_CONTRIB_PREFIX = "ghcr.io/ocx-contrib/"
_REPOSITORY_PATHS = frozenset(
    p["repository"].removeprefix(_CONTRIB_PREFIX)
    for p in REAL_PACKAGES.values()
    if p["repository"] and p["repository"].startswith(_CONTRIB_PREFIX)
)

# Findings that predate the table, generated from the test's own output; this set may only shrink.
BASELINE: frozenset[str] = frozenset(
    {
        'test/src/state_providers.py: ocx/cli:0.0.1 (setup:managed-config-onboard): unknown tag 0.0.1',
        'test/src/state_providers.py: ocx/cli:0.1.0 (setup:full-catalog): unknown tag 0.1.0',
        'website/src/content/docs/explanation/dependencies.mdx: ocx.sh/java:21: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/explanation/environments.mdx: ocx.sh/a:1: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/explanation/environments.mdx: ocx.sh/b:1: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/explanation/patches.mdx: ocx.sh/java: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/explanation/patches.mdx: ocx.sh/java:21: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/explanation/patches.mdx: ocx.sh/java:latest: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/explanation/patches.mdx: ocx.sh/javafx: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/explanation/project-groups.mdx: ocx.sh/goreleaser/goreleaser:2.0: unknown tag 2.0',
        'website/src/content/docs/explanation/project-toolchain.md: ocx.sh/ripgrep/ripgrep:14: unknown tag 14',
        'website/src/content/docs/explanation/public-index.mdx: ocx.sh/go-task:3.44: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/explanation/shell-integration.mdx: ocx.sh/ocx/cli:0.9: unknown tag 0.9',
        'website/src/content/docs/explanation/toolchain-activation.mdx: ocx.sh/shellcheck:0.11: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/guides/find-packages.mdx: ocx/mirror:1: unknown tag 1',
        'website/src/content/docs/guides/patch-packages.mdx: ocx.sh/java: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/guides/patch-packages.mdx: ocx.sh/java:21: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/guides/project-toolchain.mdx: ocx.sh/bar:1: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/guides/project-toolchain.mdx: ocx.sh/github/cli:2.40: unknown tag 2.40',
        'website/src/content/docs/guides/project-toolchain.mdx: ocx.sh/gitlab/cli:1.30: unknown tag 1.30',
        'website/src/content/docs/guides/project-toolchain.mdx: ocx.sh/goreleaser/goreleaser:2.0: unknown tag 2.0',
        'website/src/content/docs/guides/project-toolchain.mdx: ocx.sh/hyperfine:1: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/guides/project-toolchain.mdx: ocx.sh/x:1: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/integration/docker.mdx: ocx.sh/shellcheck/shellcheck:0.10: unknown tag 0.10',
        'website/src/content/docs/integration/python.mdx: ocx.sh/cpython:3.14.6: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/integration/python.mdx: ocx/black: unknown package',
        'website/src/content/docs/publishing/entry-points.md: ocx.sh/cpython:3.13: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/reference/metadata/dependencies.md: ocx.sh/java:21: ocx.sh names two segments, namespace and package',
        'website/src/content/docs/troubleshooting/shell.mdx: ocx.sh/other: ocx.sh names two segments, namespace and package',
    }
)


def _check(namespace: str, name: str, tag: str | None) -> str | None:
    """Reason an identifier is not real, or None. Placeholder namespaces are never checked."""
    if namespace in PLACEHOLDER_NAMESPACES:
        return None
    package = REAL_PACKAGES.get(f"{namespace}/{name}")
    if package is None:
        return "unknown package"
    if tag is not None and tag not in package["tags"]:
        return f"unknown tag {tag}"
    return None


def _is_file_name(name: str) -> bool:
    return name.endswith(_FILE_SUFFIXES)


def _identifier_findings(text: str) -> Iterator[tuple[str, str]]:
    """Host-qualified, tagged, untagged and physical-repository findings of ``text`` (URLs stripped)."""
    # shortcut: only `ocx.sh/` written at a token start is host-qualified-checked, a path such as
    # `symlinks/ocx.sh/<ns>/<name>` is not; widen when one slips through.
    for m in _HOST_IDENT.finditer(text):
        first, second, tag = m.groups()
        if second is None:
            if first not in _SITE_SEGMENTS and first not in PLACEHOLDER_NAMESPACES:
                yield m.group(0), "ocx.sh names two segments, namespace and package"
            continue
        reason = _check(first, second, tag)
        if reason:
            yield m.group(0), reason
    # shortcut: a tagged identifier is judged whole, so `host/ns/name:tag` is only
    # reached through its `ns/name:tag` tail; the tagged rule skips a tail that follows `/`.
    for m in _TAGGED_IDENT.finditer(text):
        namespace, name, tag = m.groups()
        if namespace in _OS_NAMESPACES or _is_file_name(name):
            continue
        if namespace == "registry" and name in ("repo", "repository"):  # the grammar placeholder
            continue
        reason = _check(namespace, name, tag)
        if reason:
            yield m.group(0), reason
    # shortcut: an untagged identifier in a namespace not in the table is not checked;
    # widen the untagged rule if one slips through.
    for m in _UNTAGGED_IDENT.finditer(text):
        namespace, name = m.groups()
        if _is_file_name(name):
            continue
        reason = _check(namespace, name, None)
        if reason:
            yield m.group(0), reason
    for m in _PHYSICAL_REPO.finditer(text):
        if m.group(1) not in _REPOSITORY_PATHS:
            yield m.group(0), "not a repository of the table"


def _output_lines(package: dict) -> set[str]:
    """Every line some recorded output of ``package`` prints, plus the host-independent form."""
    lines: set[str] = set()
    for output in package["tags"].values():
        for line in (output or "").splitlines():
            lines.add(line.strip())
            lines.add(re.sub(r"\s*\([^)]*\)$", "", line.strip()))
    return {line for line in lines if line}


def _version_output_patterns() -> list[tuple[str, re.Pattern[str], set[str]]]:
    patterns = []
    for name, package in REAL_PACKAGES.items():
        recorded = _output_lines(package)
        for line in recorded:
            if not re.search(r"[A-Za-z]", re.sub(r"\d+", "", line)):  # a bare number matches everything
                continue
            pattern = r"\d+".join(re.escape(part) for part in re.split(r"\d+", line))
            patterns.append((name, re.compile(pattern), recorded))
    return patterns


_VERSION_OUTPUT = _version_output_patterns()


def _text_fence_lines(text: str) -> Iterator[str]:
    fence: str | None = None
    for line in text.splitlines():
        m = _FENCE.match(line)
        if m:
            fence = None if fence is not None else (m.group(1) or "")
            continue
        if fence == "text" or (fence is not None and fence.startswith("text")):
            yield line.strip()


def _output_findings(text: str) -> Iterator[tuple[str, str]]:
    """A `text` fence line shaped like a table package's version output."""
    # shortcut: only a line that is exactly an output shape is judged, so output
    # embedded in a longer line is not checked; widen if one slips through.
    for line in _text_fence_lines(text):
        for name, pattern, recorded in _VERSION_OUTPUT:
            if pattern.fullmatch(line) and line not in recorded:
                yield line, f"not an output recorded for {name}"


def findings_in_text(path: str, text: str) -> set[str]:
    """Every identifier and version-output finding of one file, as ``"<path>: <identifier>: <reason>"``."""
    clean = _URL.sub("", text)
    found = {f"{path}: {ident}: {reason}" for ident, reason in _identifier_findings(clean)}
    found |= {f"{path}: {ident}: {reason}" for ident, reason in _output_findings(text)}
    return found


def declared_findings(state: str, declared: dict[str, str]) -> set[str]:
    """Every package a state declares must be a table package and tag."""
    out: set[str] = set()
    for value in declared.values():
        name, _, tag = value.partition(":")
        namespace, _, package = name.partition("/")
        reason = _check(namespace, package, tag or None)
        if reason:
            out.add(f"test/src/state_providers.py: {value} ({state}): {reason}")
    return out


def repo_tag_findings(path: str, state: str, text: str) -> set[str]:
    """``$REPO_<KEY>:<tag>`` names a tag of the package the state declares for KEY."""
    by_key = {env_key(name): name for name in DECLARED_PACKAGES.get(state, {})}
    out: set[str] = set()
    for key, tag in _SCRIPT_TAG.findall(text):
        name = by_key.get(key)
        if name is None:
            out.add(f"{path}: $REPO_{key}:{tag}: not declared by {state}")
            continue
        namespace, _, package = name.partition("/")
        reason = _check(namespace, package, tag)
        if reason:
            out.add(f"{path}: $REPO_{key}:{tag}: {reason}")
    return out


def _tracked_files() -> list[str]:
    pathspecs = [
        "website/src/content/docs/**/*.md",
        "website/src/content/docs/**/*.mdx",
        "README.md",
        "packaging/ocx/CATALOG.md",
        "test/doc_scripts/*.sh",
    ]
    listed = subprocess.run(
        ["git", "ls-files", "-z", "--", *pathspecs],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return sorted(p for p in listed.split("\0") if p and (PROJECT_ROOT / p).is_file())


def current_findings() -> set[str]:
    files = _tracked_files()
    # Floor the reader, not only the subject: an empty listing would pass vacuously.
    assert len(files) >= 150, f"git ls-files found only {len(files)} files to scan"
    found: set[str] = set()
    declared_states: set[str] = set()
    for rel in files:
        text = (PROJECT_ROOT / rel).read_text(encoding="utf-8")
        found |= findings_in_text(rel, text)
        if rel.startswith("test/doc_scripts/"):
            state = parse_doc_header(PROJECT_ROOT / rel).state
            declared_states.add(state)
            found |= repo_tag_findings(rel, state, text)
    for state in sorted(declared_states):
        found |= declared_findings(state, DECLARED_PACKAGES.get(state, {}))
    return found


@pytest.mark.parametrize(
    ("text", "red"),
    [
        pytest.param("`kitware/cmake:3.28`", True, id="red-unknown-namespace-tagged"),
        pytest.param("`nodejs/node:23`", True, id="red-unknown-tag"),
        pytest.param("ocx.sh/java:21", True, id="red-one-segment"),
        pytest.param("`astral-sh/python-build-standalone:debug-3.12`", True, id="red-unknown-variant-tag"),
        pytest.param("`astral-sh/python`", True, id="red-unknown-name-in-table-namespace"),
        pytest.param("```text\nv24.14.0\n```", True, id="red-version-output-not-recorded"),
        pytest.param("oci://ghcr.io/ocx-contrib/cmake", True, id="red-unknown-repository"),
        pytest.param("`nodejs/node:24`", False, id="green-table-tag"),
        pytest.param("`ghcr.io/acme/tool:1.0`", False, id="green-other-registry"),
        pytest.param("`acme/mytool:1.0.0`", False, id="green-placeholder-namespace"),
        pytest.param("```text\nv24.22.3\n```", False, id="green-version-output-recorded"),
        pytest.param("`linux/amd64`", False, id="green-platform"),
        pytest.param("`./.tools/node`", False, id="green-path"),
        pytest.param("`${deps.node.installPath}`", False, id="green-template"),
        pytest.param("oci://ghcr.io/ocx-contrib/nodejs/node", False, id="green-real-repository"),
        pytest.param("`nodejs/node:<tag>` and `registry/repo:tag`", False, id="green-grammar-placeholder"),
        pytest.param("`ocx.sh/nodejs/node:24@sha256:abc`", False, id="green-digest-suffix"),
        pytest.param("see https://example.com/kitware/cmake:3.28", False, id="green-url-stripped"),
    ],
)
def test_rules_red_and_green(text: str, red: bool) -> None:
    """Each rule fires on an invented identifier and stays quiet on its real twin."""
    assert bool(findings_in_text("case.md", text)) is red


@pytest.mark.parametrize(
    ("declared", "red"),
    [
        pytest.param({"kitware/cmake": "kitware/cmake:4.2.0"}, True, id="red-state-value"),
        pytest.param({"nodejs/node": "nodejs/node:22"}, False, id="green-state-value"),
    ],
)
def test_declared_state_rule_red_and_green(declared: dict[str, str], red: bool) -> None:
    assert bool(declared_findings("setup:case", declared)) is red


@pytest.mark.parametrize(
    ("script", "red"),
    [
        pytest.param('ocx index list "$REPO_NODEJS_NODE:23"', True, id="red-unknown-repo-tag"),
        pytest.param('ocx index list "$REPO_NODEJS_NODE:24"', False, id="green-known-repo-tag"),
    ],
)
def test_repo_tag_rule_red_and_green(script: str, red: bool, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setitem(DECLARED_PACKAGES, "setup:case", {"nodejs/node": "nodejs/node:22"})
    assert bool(repo_tag_findings("case.sh", "setup:case", script)) is red


def test_no_unreal_identifier_outside_the_baseline() -> None:
    """Every finding of the current tree is in ``BASELINE``; a new invented identifier is red."""
    new = sorted(current_findings() - BASELINE)
    assert not new, "findings missing from BASELINE (fix the page, or the table):\n" + "\n".join(new)



def test_baseline_has_no_stale_entries() -> None:
    """Every ``BASELINE`` entry is still a live finding, so the set shrinks with each fixed page."""
    stale = sorted(BASELINE - current_findings())
    assert not stale, "BASELINE entries that no longer occur (delete them):\n" + "\n".join(stale)


# Lines where CMake is the subject, one substring per line; each must still match a line.
_CMAKE_SUBJECT = (
    "CMake vocabulary — a memory aid",
    "[CMake][cmake-tll] uses the same three-state vocabulary",
    "This matches the CMake model",
    "Contrast with CMake's dedup-by-value model",
    "[CMake's][cmake-tll] `target_compile_definitions`",
    "[cmake-tll]: https://cmake.org/",
    "[cmake-compile-defs]: https://cmake.org/",
    "Inspired by CMake's `target_link_libraries`",
    "[CMake's PUBLIC/PRIVATE/INTERFACE link visibility][cmake-tll]",
    "[CMake][p-integrations-cmake]",
    "[p-integrations-cmake]: /integrations/cmake/",
)


def test_cmake_appears_only_where_it_is_the_subject() -> None:
    """A line naming CMake carries one of the subject substrings; the examples use real packages."""
    files = [*_tracked_files(), "website/src/data/glossary.json", "test/recordings/setups.py", "test/src/state_providers.py"]
    assert len(files) >= 150, f"only {len(files)} files to scan"
    lines = [
        (rel, n, line)
        for rel in files
        for n, line in enumerate((PROJECT_ROOT / rel).read_text(encoding="utf-8").splitlines(), 1)
        if re.search(r"(?i)cmake", line)
    ]
    assert lines, "the scan saw no CMake line at all, so its reader is broken"
    stray = [f"{rel}:{n}: {line.strip()[:100]}" for rel, n, line in lines if not any(k in line for k in _CMAKE_SUBJECT)]
    assert not stray, "CMake outside the places where it is the subject:\n" + "\n".join(stray)
    unmatched = [k for k in _CMAKE_SUBJECT if not any(k in line for _, _, line in lines)]
    assert not unmatched, "allowlist entries that match no line (delete them):\n" + "\n".join(unmatched)
