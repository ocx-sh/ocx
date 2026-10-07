# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The committed compat baseline, judged against the release tags.

The contract's `baseline/` directory holds the previous release's goldens and a
`RELEASE` file naming it (`task contract:rotate` writes both). Red when `RELEASE`
is not a `vX.Y.Z` tag on an ancestor of `HEAD`, a baseline file differs from that
tag's golden, or a newer ancestor release tag exists (rotation pending). Before
the first release whose tree has `cli.json` no baseline exists; from it on an
absent one is a pending rotation unless `HEAD` is that release commit.
"""

from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path
from typing import ClassVar

PROJECT_ROOT = Path(__file__).resolve().parents[2]

RELEASE_TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")
GOLDEN_DIR = "crates/ocx_schema/tests/golden"
CONTRACT_DIR = "crates/ocx_schema/contract"
BASELINE_DIR = f"{CONTRACT_DIR}/baseline"
BASELINE_FILES = ("cli.json", "errors.json", "reports.json")
BOOTSTRAP_MARKER = f"{GOLDEN_DIR}/cli.json"
FETCH_TAGS = "git fetch --no-tags origin '+refs/tags/v*:refs/tags/v*'"

# Set inside git hooks; they would point `git -C <tmp>` at the outer repository.
_GIT_REDIRECTS = (
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
)


def git(
    repo: Path, *args: str, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[bytes]:
    clean = {k: v for k, v in (env or os.environ).items() if k not in _GIT_REDIRECTS}
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, env=clean, check=False
    )


def rev(repo: Path, spec: str) -> str | None:
    done = git(repo, "rev-parse", "--verify", "--quiet", "--end-of-options", spec)
    return done.stdout.decode().strip() if done.returncode == 0 else None


def blob_at(repo: Path, tag: str, path: str) -> bytes | None:
    done = git(repo, "cat-file", "blob", f"refs/tags/{tag}:{path}")
    return done.stdout if done.returncode == 0 else None


def tag_commit(repo: Path, tag: str) -> str | None:
    return rev(repo, f"refs/tags/{tag}^{{commit}}")


def newest_release_tag(repo: Path) -> str | None:
    """`git describe`'s nearest `v*` tag from HEAD, skipping every one that is not `vX.Y.Z`."""
    excluded: list[str] = []
    while True:
        done = git(
            repo,
            "describe",
            "--tags",
            "--abbrev=0",
            "--match=v[0-9]*",
            *excluded,
            "HEAD",
        )
        if done.returncode != 0:
            return None
        tag = done.stdout.decode().strip()
        if RELEASE_TAG.fullmatch(tag):
            return tag
        # `--exclude` takes a glob; escaped so `v1.0-[x]` excludes itself, not a family.
        excluded.append("--exclude=" + re.sub(r"([*?\[\\])", r"\\\1", tag))


def bootstrap_tag(repo: Path) -> str | None:
    # ponytail: the goldens are never deleted once shipped, so only the newest release tag is asked.
    tag = newest_release_tag(repo)
    return (
        tag
        if tag is not None and blob_at(repo, tag, BOOTSTRAP_MARKER) is not None
        else None
    )


def baseline_problems(repo: Path) -> list[str]:
    head = rev(repo, "HEAD")
    newest = newest_release_tag(repo)
    baseline = repo / BASELINE_DIR
    if not baseline.exists():
        bootstrap = bootstrap_tag(repo)
        if bootstrap is None or tag_commit(repo, bootstrap) == head:
            return []
        return [
            (
                f"rotation pending: release {bootstrap} carries {BOOTSTRAP_MARKER} but {BASELINE_DIR}/ is absent; "
                f"run `task contract:rotate TAG={bootstrap}`"
            )
        ]

    problems = []
    present = sorted(entry.name for entry in baseline.iterdir())
    expected = sorted(("RELEASE", *BASELINE_FILES))
    if present != expected:
        problems.append(f"{BASELINE_DIR}/ holds {present}, expected exactly {expected}")

    marker = baseline / "RELEASE"
    release = (
        marker.read_text(encoding="utf-8").removesuffix("\n")
        if marker.is_file()
        else ""
    )
    if not RELEASE_TAG.fullmatch(release):
        return [*problems, f"RELEASE {release!r} is not a vX.Y.Z tag name"]
    commit = tag_commit(repo, release)
    if commit is None:
        return [*problems, f"RELEASE {release} names no tag"]
    ancestor = git(repo, "merge-base", "--is-ancestor", commit, "HEAD").returncode == 0
    if not ancestor:
        problems.append(f"RELEASE {release} is not an ancestor of HEAD")

    for name in BASELINE_FILES:
        golden = blob_at(repo, release, f"{GOLDEN_DIR}/{name}")
        copy = baseline / name
        if golden is None:
            problems.append(f"{release} has no {GOLDEN_DIR}/{name}")
        elif not copy.is_file() or copy.read_bytes() != golden:
            problems.append(f"{BASELINE_DIR}/{name} differs from {release}'s golden")

    if (
        ancestor
        and newest is not None
        and newest != release
        and tag_commit(repo, newest) != head
    ):
        problems.append(
            f"rotation pending: {newest} is newer than RELEASE {release}; run `task contract:rotate TAG={newest}`"
        )
    return problems


# --- the repository itself --------------------------------------------------


def test_the_checkout_reaches_a_release_tag() -> None:
    """Reader floor: with no tags every rule above reads "no bootstrap" and passes."""
    assert newest_release_tag(PROJECT_ROOT) is not None, (
        f"no vX.Y.Z tag reachable from HEAD; run `{FETCH_TAGS}` (add `--unshallow` on a shallow clone)"
    )


def test_the_repository_baseline_matches_the_release_tags() -> None:
    assert baseline_problems(PROJECT_ROOT) == []


# --- a scratch repository ---------------------------------------------------


class Repo:
    """A throwaway git repository with deterministic commits."""

    ENV: ClassVar[dict[str, str]] = {
        **os.environ,
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_AUTHOR_NAME": "lint",
        "GIT_AUTHOR_EMAIL": "lint@example.invalid",
        "GIT_COMMITTER_NAME": "lint",
        "GIT_COMMITTER_EMAIL": "lint@example.invalid",
    }

    def __init__(self, path: Path) -> None:
        self.path = path
        self.run("init", "--quiet", "--template=", "--initial-branch=main")

    def run(self, *args: str) -> str:
        done = git(self.path, *args, env=self.ENV)
        assert done.returncode == 0, f"git {' '.join(args)}: {done.stderr.decode()}"
        return done.stdout.decode().strip()

    def commit(
        self, files: dict[str, str | bytes | None], message: str = "change"
    ) -> str:
        for name, content in files.items():
            target = self.path / name
            if content is None:
                target.unlink()
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(
                content.encode() if isinstance(content, str) else content
            )
        self.run("add", "--all")
        self.run("commit", "--quiet", "--allow-empty", "-m", message)
        return self.run("rev-parse", "HEAD")

    def tag(self, name: str, *, annotated: bool = False) -> None:
        self.run("tag", *(["-a", "-m", name] if annotated else []), name)


GOLDENS = {
    f"{GOLDEN_DIR}/{name}": f'{{"doc": "{name}", "release": 1}}\n'
    for name in BASELINE_FILES
}


def rotate(repo: Repo, tag: str) -> None:
    """What `task contract:rotate TAG=<tag>` writes, committed."""
    files: dict[str, str | bytes | None] = {
        f"{BASELINE_DIR}/{name}": git(
            repo.path, "show", f"refs/tags/{tag}:{GOLDEN_DIR}/{name}"
        ).stdout
        for name in BASELINE_FILES
    }
    files[f"{BASELINE_DIR}/RELEASE"] = f"{tag}\n"
    repo.commit(files, f"chore(contract): rotate the baseline to {tag}")


def released(tmp_path: Path, tag: str = "v1.0.0") -> Repo:
    """A repository whose release `tag` carries the goldens, rotated, one commit on."""
    repo = Repo(tmp_path)
    repo.commit(GOLDENS, "release")
    repo.tag(tag, annotated=True)
    rotate(repo, tag)
    repo.commit({"README": "work\n"})
    return repo


def test_no_release_tag_and_no_baseline_is_green(tmp_path: Path) -> None:
    repo = Repo(tmp_path)
    repo.commit({"README": "x\n"})
    assert baseline_problems(repo.path) == []


def test_a_release_before_the_goldens_needs_no_baseline(tmp_path: Path) -> None:
    repo = Repo(tmp_path)
    repo.commit({"README": "x\n"})
    repo.tag("v0.6.4")
    repo.commit(GOLDENS)
    assert baseline_problems(repo.path) == []


def test_bootstrap_tag_without_baseline_is_rotation_pending(tmp_path: Path) -> None:
    repo = Repo(tmp_path)
    repo.commit(GOLDENS)
    repo.tag("v0.7.0")
    repo.commit({"README": "x\n"})
    assert baseline_problems(repo.path) == [
        (
            f"rotation pending: release v0.7.0 carries {BOOTSTRAP_MARKER} but {BASELINE_DIR}/ is absent; "
            "run `task contract:rotate TAG=v0.7.0`"
        )
    ]


def test_the_bootstrap_release_commit_itself_is_green(tmp_path: Path) -> None:
    repo = Repo(tmp_path)
    repo.commit(GOLDENS)
    repo.tag("v0.7.0")
    assert baseline_problems(repo.path) == []


def test_a_fresh_rotation_is_green(tmp_path: Path) -> None:
    assert baseline_problems(released(tmp_path).path) == []


def test_a_byte_flip_in_the_baseline_is_red(tmp_path: Path) -> None:
    repo = released(tmp_path)
    target = repo.path / BASELINE_DIR / "errors.json"
    flipped = target.read_bytes().replace(b'"release": 1', b'"release": 2')
    assert flipped != target.read_bytes(), "mutation did not land"
    target.write_bytes(flipped)
    assert baseline_problems(repo.path) == [
        f"{BASELINE_DIR}/errors.json differs from v1.0.0's golden"
    ]


def test_release_naming_a_branch_is_red(tmp_path: Path) -> None:
    repo = released(tmp_path)
    repo.run("branch", "v1.0.1")
    repo.commit({f"{BASELINE_DIR}/RELEASE": "v1.0.1\n"})
    assert baseline_problems(repo.path) == ["RELEASE v1.0.1 names no tag"]


def test_a_malformed_release_is_red(tmp_path: Path) -> None:
    repo = released(tmp_path)
    repo.tag("v1.0")
    repo.commit({f"{BASELINE_DIR}/RELEASE": "v1.0\n"})
    assert baseline_problems(repo.path) == ["RELEASE 'v1.0' is not a vX.Y.Z tag name"]


def test_a_release_off_the_ancestry_is_red(tmp_path: Path) -> None:
    repo = released(tmp_path)
    repo.run("checkout", "--quiet", "-b", "side")
    repo.commit({"SIDE": "x\n"})
    repo.tag("v9.0.0")
    repo.run("checkout", "--quiet", "main")
    repo.commit({f"{BASELINE_DIR}/RELEASE": "v9.0.0\n"})
    assert baseline_problems(repo.path) == ["RELEASE v9.0.0 is not an ancestor of HEAD"]


def test_an_extra_baseline_file_is_red(tmp_path: Path) -> None:
    repo = released(tmp_path)
    repo.commit({f"{BASELINE_DIR}/ledger.toml": ""})
    assert baseline_problems(repo.path) == [
        (
            f"{BASELINE_DIR}/ holds ['RELEASE', 'cli.json', 'errors.json', 'ledger.toml', 'reports.json'], "
            "expected exactly ['RELEASE', 'cli.json', 'errors.json', 'reports.json']"
        )
    ]


def test_a_newer_release_tag_is_rotation_pending(tmp_path: Path) -> None:
    repo = released(tmp_path)
    repo.tag("v1.1.0")
    repo.commit({"README": "more\n"})
    assert baseline_problems(repo.path) == [
        "rotation pending: v1.1.0 is newer than RELEASE v1.0.0; run `task contract:rotate TAG=v1.1.0`"
    ]


def test_the_newer_release_commit_itself_is_green(tmp_path: Path) -> None:
    repo = released(tmp_path)
    repo.tag("v1.1.0")
    assert baseline_problems(repo.path) == []


def test_rotating_to_the_newer_release_is_green(tmp_path: Path) -> None:
    repo = released(tmp_path)
    repo.commit({f"{GOLDEN_DIR}/cli.json": '{"doc": "cli.json", "release": 2}\n'})
    repo.tag("v1.1.0")
    rotate(repo, "v1.1.0")
    assert baseline_problems(repo.path) == []


def test_non_release_tags_never_count(tmp_path: Path) -> None:
    repo = released(tmp_path)
    repo.tag("backup/main-pre-finalize")
    repo.tag("v2.0.0-rc.1")
    repo.tag("0.3.1")
    repo.commit({"README": "more\n"})
    assert baseline_problems(repo.path) == []
