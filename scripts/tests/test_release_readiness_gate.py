"""`release_readiness_gate.py`'s proofs: a stub `gh` on PATH, no network.

Also drives the real `release:tag` task body in a throwaway git repo, so "every non-zero refuses"
is shown on the task a releaser runs, not only on the script's exit codes.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
import release_readiness_gate as gate

REPO_ROOT = Path(__file__).resolve().parents[2]
SHA = "a" * 40
OTHER = "b" * 40

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="the stub `gh` is a POSIX shell script")


def _run(sha: str = SHA, stdout: str = "[]", stderr: str = "", code: int = 0, *, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> int:
    """Run the gate against a `gh` that prints `stdout`/`stderr` and exits `code`; argv lands in `gh.argv`."""
    stub_dir = tmp_path / "bin"
    stub_dir.mkdir(exist_ok=True)
    (tmp_path / "out").write_text(stdout, encoding="utf-8")
    (tmp_path / "err").write_text(stderr, encoding="utf-8")
    stub = stub_dir / "gh"
    stub.write_text(
        f'#!/bin/sh\nprintf "%s\\n" "$@" > "{tmp_path}/gh.argv"\ncat "{tmp_path}/out"\ncat "{tmp_path}/err" >&2\nexit {code}\n',
        encoding="utf-8",
    )
    stub.chmod(0o755)
    monkeypatch.setenv("PATH", f"{stub_dir}{os.pathsep}/usr/bin{os.pathsep}/bin")  # the stub shells out to cat
    return gate.run(sha)


def _runs(*runs: dict) -> str:
    return json.dumps(list(runs))


def _green(sha: str = SHA, title: str = f"readiness {SHA}") -> dict:
    return {"url": "https://example.test/run/1", "displayTitle": title, "headSha": sha, "conclusion": "success"}


def test_a_successful_run_for_the_sha_exits_0_and_names_it(tmp_path, monkeypatch, capsys) -> None:
    assert _run(stdout=_runs(_green()), tmp_path=tmp_path, monkeypatch=monkeypatch) == 0
    out = capsys.readouterr().out
    assert "https://example.test/run/1" in out
    assert f"readiness {SHA}" in out
    assert "BYPASS" not in out


def test_the_query_is_for_this_workflow_commit_and_status(tmp_path, monkeypatch) -> None:
    _run(stdout=_runs(_green()), tmp_path=tmp_path, monkeypatch=monkeypatch)
    argv = (tmp_path / "gh.argv").read_text(encoding="utf-8").split()
    assert argv[:2] == ["run", "list"]
    for flag, value in (("--workflow", "release-readiness.yml"), ("--commit", SHA), ("--status", "success")):
        assert argv[argv.index(flag) + 1] == value


def test_a_bypass_run_is_accepted_and_its_reason_is_printed(tmp_path, monkeypatch, capsys) -> None:
    title = f"readiness {SHA} BYPASS: runner outage, hotfix"
    assert _run(stdout=_runs(_green(title=title)), tmp_path=tmp_path, monkeypatch=monkeypatch) == 0
    out = capsys.readouterr().out
    assert "BYPASS: runner outage, hotfix" in out
    assert "is a BYPASS" in out


@pytest.mark.parametrize(
    "runs",
    [
        pytest.param([], id="no-run-at-all"),
        pytest.param([_green(sha=OTHER)], id="a-run-for-another-commit"),
        pytest.param([{**_green(), "conclusion": "failure"}], id="a-red-run-that-a-flag-let-through"),
        pytest.param([{**_green(), "conclusion": "cancelled"}], id="a-cancelled-run-that-a-flag-let-through"),
        pytest.param([{**_green(), "conclusion": "skipped"}], id="a-skipped-run-that-a-flag-let-through"),
        pytest.param([{**_green(), "conclusion": ""}], id="an-unfinished-run-that-a-flag-let-through"),
    ],
)
def test_no_successful_run_for_the_sha_exits_1(runs, tmp_path, monkeypatch, capsys) -> None:
    assert _run(stdout=json.dumps(runs), tmp_path=tmp_path, monkeypatch=monkeypatch) == 1
    assert "no successful release-readiness.yml run" in capsys.readouterr().err


def test_a_missing_gh_exits_69(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("PATH", str(tmp_path))
    assert gate.run(SHA) == 69


@pytest.mark.parametrize(
    "stderr",
    [
        "To get started with GitHub CLI, please run:  gh auth login\n",
        "gh: To use GitHub CLI in automation, set the GH_TOKEN environment variable.\n",
        "HTTP 401: Bad credentials (https://api.github.com/graphql)\n",
    ],
)
def test_an_unauthenticated_gh_exits_69(stderr, tmp_path, monkeypatch) -> None:
    assert _run(stderr=stderr, code=4, tmp_path=tmp_path, monkeypatch=monkeypatch) == 69


@pytest.mark.parametrize(
    ("stdout", "stderr", "code"),
    [
        pytest.param("", "HTTP 502: Bad Gateway\n", 1, id="gh-fails-for-another-reason"),
        pytest.param("not json", "", 0, id="output-is-not-json"),
        pytest.param('[{"url": "u"}]', "", 0, id="a-run-without-the-fields-read"),
        pytest.param('{"runs": []}', "", 0, id="json-of-the-wrong-shape"),
    ],
)
def test_any_other_failure_is_a_distinct_non_zero(stdout, stderr, code, tmp_path, monkeypatch) -> None:
    assert _run(stdout=stdout, stderr=stderr, code=code, tmp_path=tmp_path, monkeypatch=monkeypatch) == 74


@pytest.mark.parametrize("sha", ["HEAD", "main", "abc123", "A" * 40, SHA + "0", ""])
def test_a_sha_that_is_not_a_full_commit_is_refused_before_gh_runs(sha, monkeypatch) -> None:
    monkeypatch.setenv("PATH", "")
    with pytest.raises(SystemExit) as refused:
        gate.main(["--sha", sha])
    assert refused.value.code == 2


# ---------------------------------------------------------------------------
# `task release:tag`, the real task body, in a throwaway repo with a bare remote
# ---------------------------------------------------------------------------


def _git(cwd: Path, *args: str) -> str:
    done = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, encoding="utf-8", check=True,
        env={**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"},
    )  # fmt: skip
    return done.stdout.strip()


@pytest.fixture
def release_repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> dict[str, Path]:
    """A repo on `main` holding a `release: v1.2.3` commit, a bare `origin`, a stub `gh`, and a Taskfile including release.taskfile.yml."""
    if shutil.which("task") is None:
        pytest.fail("no `task` on PATH — run through `task scripts:self-test`")
    origin = tmp_path / "origin.git"
    subprocess.run(["git", "init", "--bare", "-q", "-b", "main", str(origin)], check=True)
    work = tmp_path / "work"
    work.mkdir()
    _git(work, "init", "-q", "-b", "main")
    _git(work, "remote", "add", "origin", str(origin))
    (work / "scripts").mkdir()
    shutil.copy(REPO_ROOT / "scripts" / "release_readiness_gate.py", work / "scripts")
    (work / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
    (work / "taskfile.yml").write_text(
        f"version: '3'\nincludes:\n  release: {REPO_ROOT / 'taskfiles' / 'release.taskfile.yml'}\n", encoding="utf-8"
    )
    _git(work, "add", "-A")
    _git(work, "commit", "-q", "-m", "release: v1.2.3")
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    return {"work": work, "origin": origin, "bin": bin_dir}


def _stub_gh(bin_dir: Path, stdout: str, code: int = 0, stderr: str = "") -> None:
    stub = bin_dir / "gh"
    stub.write_text(f"#!/bin/sh\ncat <<'EOF'\n{stdout}\nEOF\ncat >&2 <<'EOF'\n{stderr}\nEOF\nexit {code}\n", encoding="utf-8")
    stub.chmod(0o755)


def _release_tag(work: Path) -> subprocess.CompletedProcess[str]:
    # `-x` hands back the exit code of the script's failing command (1, 69, 74), not go-task's own 201.
    return subprocess.run(["task", "-x", "release:tag"], cwd=work, capture_output=True, text=True, encoding="utf-8", check=False)


def test_release_tag_tags_and_pushes_atomically_once_the_gate_is_green(release_repo) -> None:
    work, origin = release_repo["work"], release_repo["origin"]
    head = _git(work, "rev-parse", "HEAD")
    _stub_gh(release_repo["bin"], _runs(_green(sha=head, title=f"readiness {head}")))
    done = _release_tag(work)
    assert done.returncode == 0, done.stderr
    assert _git(origin, "rev-parse", "refs/tags/v1.2.3^{commit}") == head
    assert _git(origin, "rev-parse", "refs/heads/main") == head


@pytest.mark.parametrize(
    ("stdout", "stderr", "code", "exit_code", "why"),
    [
        pytest.param("[]", "", 0, 1, "no successful release-readiness.yml run", id="exit-1-no-run"),
        pytest.param("", "To get started, please run:  gh auth login", 4, 69, "gh run list failed (4)", id="exit-69-unauthenticated"),
        pytest.param("", "HTTP 502: Bad Gateway", 1, 74, "HTTP 502: Bad Gateway", id="exit-74-gh-failed"),
        pytest.param("not json", "", 0, 74, "unexpected `gh run list` output", id="exit-74-garbage"),
    ],
)
def test_release_tag_refuses_on_every_non_zero_and_leaves_no_tag(release_repo, stdout, stderr, code, exit_code, why) -> None:
    work, origin = release_repo["work"], release_repo["origin"]
    _stub_gh(release_repo["bin"], stdout, code, stderr)
    done = _release_tag(work)
    assert done.returncode == exit_code, (done.returncode, done.stderr)
    assert why in done.stderr, done.stderr
    assert _git(work, "tag", "--list") == ""
    assert _git(origin, "tag", "--list") == ""
    assert _git(origin, "branch", "--list") == ""


def test_release_tag_refuses_a_head_that_is_not_the_release_commit(release_repo) -> None:
    work, origin = release_repo["work"], release_repo["origin"]
    (work / "x").write_text("x", encoding="utf-8")
    _git(work, "add", "-A")
    _git(work, "commit", "-q", "-m", "feat: something else")
    head = _git(work, "rev-parse", "HEAD")
    _stub_gh(release_repo["bin"], _runs(_green(sha=head)))
    done = _release_tag(work)
    assert done.returncode != 0
    assert "release: v1.2.3" in done.stderr
    assert _git(origin, "tag", "--list") == ""
