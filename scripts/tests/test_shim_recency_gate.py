"""The recency step of `build-windows-shims.yml`: green when the blob is at or after the sources, red when stale
or when the source pathspec matches nothing (a gate that compared nothing must not pass)."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
STEP = "Committed blobs must be re-cut when shim sources change"
BLOB = "crates/ocx_store/src/shims/ocx-shim-x86_64-unknown-linux-musl"


def step_script() -> str:
    workflow = yaml.safe_load((ROOT / ".github/workflows/build-windows-shims.yml").read_text(encoding="utf-8"))
    steps = workflow["jobs"]["build"]["steps"]
    return next(s["run"] for s in steps if s.get("name") == STEP)


def commit(repo: Path, path: str, text: str) -> None:
    target = repo / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text, encoding="utf-8")
    git = ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid"]
    subprocess.run([*git, "add", path], cwd=repo, check=True)
    subprocess.run([*git, "commit", "-q", "--no-verify", "-m", f"c {path}"], cwd=repo, check=True)


def run_step(repo: Path) -> subprocess.CompletedProcess[str]:
    script = repo / "step.sh"
    script.write_text(step_script(), encoding="utf-8")
    env = {**os.environ, "COMMITTED": BLOB}
    return subprocess.run(["bash", str(script)], cwd=repo, env=env, capture_output=True, text=True)


def make_repo(tmp_path: Path) -> Path:
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    return tmp_path


def test_a_blob_cut_after_the_sources_passes(tmp_path: Path) -> None:
    repo = make_repo(tmp_path)
    commit(repo, "crates/ocx_shim/src/main.rs", "a")
    commit(repo, BLOB, "b")
    result = run_step(repo)
    assert result.returncode == 0, result.stdout + result.stderr


def test_a_blob_older_than_the_sources_fails(tmp_path: Path) -> None:
    repo = make_repo(tmp_path)
    commit(repo, BLOB, "b")
    commit(repo, "crates/ocx_shim/src/main.rs", "a")
    result = run_step(repo)
    assert result.returncode == 1, result.stdout + result.stderr


def test_a_source_pathspec_that_matches_nothing_fails_and_names_the_paths(tmp_path: Path) -> None:
    repo = make_repo(tmp_path)
    commit(repo, BLOB, "b")
    result = run_step(repo)
    assert result.returncode == 1, result.stdout + result.stderr
    assert "crates/ocx_shim/" in result.stdout + result.stderr
    assert "crates/ocx_env_apply/" in result.stdout + result.stderr
