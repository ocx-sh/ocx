# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx.toml`` content preservation across mutations.

Regression net for issue #256: ``ocx add`` / ``ocx remove`` re-serialized the
parsed config over the whole file, which discarded every comment (including the
``#:schema`` directive ``ocx init`` writes), reordered the bindings
alphabetically, and materialized empty ``[group]`` / ``[package]`` tables.

The contract these tests pin: a mutation touches only the key it mutates;
everything else in the file comes back byte-for-byte.
"""
from __future__ import annotations

import difflib
import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import PackageInfo, make_package
from src.runner import OcxRunner


EXIT_SUCCESS = 0

SCHEMA_DIRECTIVE = "#:schema https://ocx.sh/schemas/project/v1.json"


def _run_cmd(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _project(tmp_path: Path, body: str) -> Path:
    project_dir = tmp_path / "proj"
    project_dir.mkdir(exist_ok=True)
    (project_dir / "ocx.toml").write_text(body)
    return project_dir


def _publish(ocx: OcxRunner, tmp_path: Path, suffix: str) -> PackageInfo:
    """Publish a throwaway package whose repo name carries ``suffix``."""
    repo = f"t_{uuid4().hex[:8]}_{suffix}"
    return make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)


def _assert_contains_all(content: str, fragments: list[str], context: str) -> None:
    missing = [fragment for fragment in fragments if fragment not in content]
    assert not missing, f"{context} lost {missing!r}; got:\n{content}"


def test_add_preserves_comments_and_schema_directive(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Every comment survives ``ocx add`` — the ``#:schema`` directive on the
    first line (the taplo editor hook) as much as a trailing note on a binding.
    """
    kept = _publish(ocx, tmp_path, "keep")
    added = _publish(ocx, tmp_path, "add")

    body = (
        f"{SCHEMA_DIRECTIVE}\n"
        "# OCX project toolchain\n"
        "\n"
        "[tools]\n"
        "# pinned deliberately, do not bump\n"
        f'{kept.repo} = "{kept.fq}"  # trailing note\n'
    )
    project_dir = _project(tmp_path, body)

    result = _run_cmd(ocx, project_dir, "add", added.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    assert content.splitlines()[0] == SCHEMA_DIRECTIVE, (
        f"the #:schema directive must remain the first line; got:\n{content}"
    )
    _assert_contains_all(
        content,
        [
            "# OCX project toolchain",
            "# pinned deliberately, do not bump",
            "# trailing note",
            added.repo,
        ],
        "ocx add",
    )


def test_add_inserts_exactly_one_line(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx add`` is an insertion, not a rewrite: the diff against the
    pre-mutation file is exactly one added line and no removed line."""
    kept = _publish(ocx, tmp_path, "diffkeep")
    added = _publish(ocx, tmp_path, "diffadd")

    body = (
        f"{SCHEMA_DIRECTIVE}\n"
        "\n"
        "[tools]\n"
        f'{kept.repo} = "{kept.fq}"\n'
    )
    project_dir = _project(tmp_path, body)

    result = _run_cmd(ocx, project_dir, "add", added.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    after = (project_dir / "ocx.toml").read_text()
    diff = list(
        difflib.unified_diff(
            body.splitlines(), after.splitlines(), lineterm="", n=0
        )
    )
    added_lines = [line for line in diff if line.startswith("+") and not line.startswith("+++")]
    removed_lines = [line for line in diff if line.startswith("-") and not line.startswith("---")]

    assert len(added_lines) == 1, f"expected exactly one added line; diff:\n" + "\n".join(diff)
    assert added.repo in added_lines[0], (
        f"the added line must be the new binding; got {added_lines[0]!r}"
    )
    assert removed_lines == [], f"ocx add must remove nothing; diff:\n" + "\n".join(diff)


def test_add_emits_no_empty_group_or_package_tables(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A default-group add never materializes the empty ``[group]`` /
    ``[package]`` headers the struct rewrite used to emit."""
    added = _publish(ocx, tmp_path, "notables")
    project_dir = _project(tmp_path, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", added.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    lines = (project_dir / "ocx.toml").read_text().splitlines()
    assert "[group]" not in lines, f"empty [group] table must not appear; got:\n{lines}"
    assert "[package]" not in lines, f"empty [package] table must not appear; got:\n{lines}"


def test_add_preserves_declaration_order(ocx: OcxRunner, tmp_path: Path) -> None:
    """Existing bindings keep the order the user wrote them in, and the new
    binding is appended last — not sorted into place."""
    short = uuid4().hex[:8]
    zeta = make_package(ocx, f"t_{short}_zeta", "1.0.0", tmp_path, new=True, cascade=False)
    alpha = make_package(ocx, f"t_{short}_alpha", "1.0.0", tmp_path, new=True, cascade=False)
    added = _publish(ocx, tmp_path, "ordered")

    body = (
        "[tools]\n"
        f'{zeta.repo} = "{zeta.fq}"\n'
        f'{alpha.repo} = "{alpha.fq}"\n'
    )
    project_dir = _project(tmp_path, body)

    result = _run_cmd(ocx, project_dir, "add", added.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    assert content.index(zeta.repo) < content.index(alpha.repo), (
        f"declaration order must survive the mutation; got:\n{content}"
    )
    assert content.index(added.repo) > content.index(alpha.repo), (
        f"a new binding is appended, not sorted in; got:\n{content}"
    )


def test_add_to_group_preserves_comments(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx add --group`` writes the group binding without disturbing the
    file's comments and without emitting a bare ``[group]`` header."""
    added = _publish(ocx, tmp_path, "grouped")

    body = f"{SCHEMA_DIRECTIVE}\n# keep me\n\n[tools]\n"
    project_dir = _project(tmp_path, body)

    result = _run_cmd(ocx, project_dir, "add", "--group", "ci", added.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add --group failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    assert content.splitlines()[0] == SCHEMA_DIRECTIVE, (
        f"the #:schema directive must remain the first line; got:\n{content}"
    )
    _assert_contains_all(content, ["# keep me", added.repo], "ocx add --group")
    assert "[group]" not in content.splitlines(), (
        f"a bare [group] header must not appear; got:\n{content}"
    )
    assert "[package]" not in content.splitlines(), (
        f"an empty [package] table must not appear; got:\n{content}"
    )


def test_remove_preserves_surviving_comments(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx remove`` deletes one key and leaves every other line — comments
    included — exactly as the user wrote it."""
    kept = _publish(ocx, tmp_path, "remkeep")
    dropped = _publish(ocx, tmp_path, "remdrop")

    body = (
        f"{SCHEMA_DIRECTIVE}\n"
        "# toolchain notes\n"
        "\n"
        "[tools]\n"
        f'{kept.repo} = "{kept.fq}"  # keep this one\n'
        f'{dropped.repo} = "{dropped.fq}"\n'
    )
    project_dir = _project(tmp_path, body)

    lock = _run_cmd(ocx, project_dir, "lock")
    assert lock.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock.stderr!r}"

    result = _run_cmd(ocx, project_dir, "remove", dropped.repo)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx remove failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    assert dropped.repo not in content, (
        f"the removed binding must be gone; got:\n{content}"
    )
    assert content.splitlines()[0] == SCHEMA_DIRECTIVE, (
        f"the #:schema directive must remain the first line; got:\n{content}"
    )
    _assert_contains_all(
        content, ["# toolchain notes", "# keep this one", kept.repo], "ocx remove"
    )


def test_add_then_remove_round_trips_byte_identical(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Adding a binding and removing it again returns the file to its original
    bytes — the strongest statement of "mutations touch only their own key"."""
    kept = _publish(ocx, tmp_path, "rtkeep")
    transient = _publish(ocx, tmp_path, "rttransient")

    body = (
        f"{SCHEMA_DIRECTIVE}\n"
        "# round-trip fixture\n"
        "\n"
        "[tools]\n"
        f'{kept.repo} = "{kept.fq}"\n'
    )
    project_dir = _project(tmp_path, body)

    add_result = _run_cmd(ocx, project_dir, "add", transient.fq)
    assert add_result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={add_result.returncode}, stderr={add_result.stderr!r}"
    )
    remove_result = _run_cmd(ocx, project_dir, "remove", transient.repo)
    assert remove_result.returncode == EXIT_SUCCESS, (
        f"ocx remove failed: rc={remove_result.returncode}, stderr={remove_result.stderr!r}"
    )

    assert (project_dir / "ocx.toml").read_text() == body, (
        "add followed by remove must restore the original bytes; got:\n"
        f"{(project_dir / 'ocx.toml').read_text()}"
    )


def test_add_preserves_env_and_package_sections(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Sections the mutation does not target — ``[env]``, ``[group.<n>.env]``,
    ``[package."<id>"]`` — come back verbatim after an unrelated add."""
    kept = _publish(ocx, tmp_path, "envkeep")
    added = _publish(ocx, tmp_path, "envadd")

    tail = (
        "[env]\n"
        'PROJECT_FLAG = "1"  # project-wide\n'
        "\n"
        "[group.ci.env]\n"
        'CI_FLAG = "yes"\n'
        "\n"
        f'[package."{ocx.registry}/{kept.repo}"]\n'
        "no-patches = true\n"
    )
    body = f"[tools]\n{kept.repo} = \"{kept.fq}\"\n\n{tail}"
    project_dir = _project(tmp_path, body)

    result = _run_cmd(ocx, project_dir, "add", added.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    assert tail in content, (
        f"the untouched [env] / [group.*.env] / [package.*] block must be preserved "
        f"verbatim; got:\n{content}"
    )


def test_init_then_add_keeps_schema_directive_first(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The file ``ocx init`` generates keeps its schema directive through the
    first ``ocx add``, and carries no unparsable ``registry`` hint."""
    added = _publish(ocx, tmp_path, "initadd")

    project_dir = tmp_path / "proj"
    project_dir.mkdir(exist_ok=True)
    init_result = _run_cmd(ocx, project_dir, "init")
    assert init_result.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={init_result.returncode}, stderr={init_result.stderr!r}"
    )

    generated = (project_dir / "ocx.toml").read_text()
    assert "registry" not in generated, (
        "ocx init must not advertise a `registry` key — ocx.toml has none and "
        f"the parser rejects it; got:\n{generated}"
    )

    result = _run_cmd(ocx, project_dir, "add", added.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    assert content.splitlines()[0] == SCHEMA_DIRECTIVE, (
        f"the #:schema directive must survive the first add; got:\n{content}"
    )


def test_global_add_preserves_comments(ocx: OcxRunner, tmp_path: Path) -> None:
    """The global toolchain file gets the same treatment as a project file."""
    added = _publish(ocx, tmp_path, "globaladd")

    global_toml = Path(ocx.ocx_home) / "ocx.toml"
    body = f"{SCHEMA_DIRECTIVE}\n# global toolchain\n\n[tools]\n"
    global_toml.write_text(body)

    result = _run_cmd(ocx, tmp_path, "--global", "add", added.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --global add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = global_toml.read_text()
    assert content.splitlines()[0] == SCHEMA_DIRECTIVE, (
        f"the #:schema directive must remain the first line; got:\n{content}"
    )
    _assert_contains_all(content, ["# global toolchain", added.repo], "ocx --global add")
