"""Pinned-id flow + offline exec acceptance test (Phase 11 §S5).

Locks the post-pin contract: `ocx lock` + `ocx pull` produces a project state
where `OCX_OFFLINE=1 ocx exec` resolves every tool from its pinned digest,
without consulting the local tag store and without contacting a registry.

The contract under test is the union of three commits:

- `Op::Query` flip (commit 2): query commands no longer fill the local tag
  store, so a freshly-locked project has no tag pointers to fall back to.
- `persist_manifest_chain` / `commit_tag` split (commit 3): pull persists
  manifest blobs.
- Pinned-id tag-skip (commit 4): pull does NOT commit tag pointers for
  digest-bearing identifiers, so `ocx.lock` is the sole authoritative
  record of the tag→digest mapping.

If the test fails, the pinned-id flow has regressed in one of those layers
or the offline-exec resolve path has started reaching for the tag store
again.
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

from src import OcxRunner
from src.helpers import make_package


def _write_ocx_toml(project: Path, body: str) -> Path:
    path = project / "ocx.toml"
    path.write_text(body)
    return path


def _run(ocx: OcxRunner, *args: str, cwd: Path, extra_env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
    )


def test_pinned_id_exec_offline_succeeds(ocx: OcxRunner, tmp_path: Path) -> None:
    """Lock + pull online; OCX_OFFLINE=1 ocx exec resolves via pinned digest."""
    short_id = uuid4().hex[:8]
    repo = f"t_{short_id}_pinned_offline"
    tag = "1.0.0"
    bin_name = "hello"

    pkg = make_package(ocx, repo, tag, tmp_path, new=True, cascade=False, bins=[bin_name])

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, f"""\
[tools]
{bin_name} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = _run(ocx, "lock", cwd=project)
    assert lock.returncode == 0, f"ocx lock failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    assert (project / "ocx.lock").exists(), "ocx lock must produce ocx.lock"

    pull = _run(ocx, "pull", cwd=project)
    assert pull.returncode == 0, f"ocx pull failed: rc={pull.returncode}\nstderr:\n{pull.stderr}"

    # Read the pinned digest from ocx.lock so the offline exec uses a
    # digest-bearing identifier (no tag-store reach-back needed).
    import tomllib
    lock_data = tomllib.loads((project / "ocx.lock").read_text())
    locked_tool = next(t for t in lock_data["tool"] if t["name"] == bin_name)
    pinned_id = locked_tool["pinned"]

    # Wipe the local tag store. The pinned-id flow must not need it for
    # exec — any reach-back to the tag store would fail this assertion
    # because the directory is gone.
    tags_root = Path(ocx.env["OCX_HOME"]) / "tags"
    if tags_root.exists():
        shutil.rmtree(tags_root)

    # Offline exec via the digest-pinned identifier. Resolution must
    # succeed using only the persisted manifest blobs from `ocx pull`.
    exec_result = _run(
        ocx,
        "exec",
        pinned_id,
        "--",
        bin_name,
        cwd=project,
        extra_env={"OCX_OFFLINE": "1"},
    )
    assert exec_result.returncode == 0, (
        f"offline exec failed: rc={exec_result.returncode}\n"
        f"stderr:\n{exec_result.stderr}\nstdout:\n{exec_result.stdout}"
    )
    assert pkg.marker in exec_result.stdout, (
        f"expected marker {pkg.marker!r} in offline exec output; got: {exec_result.stdout!r}"
    )
    assert not tags_root.exists(), (
        "offline exec must not recreate the local tag store under pinned-id flow"
    )
