# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The CWD walk never adopts ``$OCX_HOME/ocx.toml`` as a project (#485).

``$OCX_HOME/ocx.toml`` is the **global toolchain manifest**, reachable only
through the explicit ``--global`` selector
(``adr_global_toolchain_tier.md`` §Decision 1). The CWD walk used to return it
as an ordinary discovery hit, so any command run from a working directory
*inside* the store silently acquired the global toolchain as its project.

Sibling of the home-tier rows in ``test_project_config.py``, which observe the
walk from a directory *beside* ``$OCX_HOME``. This module needs the frozen
``src.toolchain_fixtures.run_in`` helper to run ``ocx`` from a chosen working
directory, which is why it is its own module rather than two more rows there.
"""
from __future__ import annotations

import json
from pathlib import Path

from src.runner import OcxRunner
from src.toolchain_fixtures import run_in

EXIT_USAGE = 64
"""``ProjectContextError::NoProject`` — "no ocx.toml found"."""


def test_cwd_inside_ocx_home_is_not_a_project(ocx: OcxRunner, tmp_path: Path) -> None:
    """S-010 / C-010 — a cwd under ``$OCX_HOME`` finds no project, and
    ``--global`` still reaches the toolchain from that same cwd.

    Both halves are asserted here on purpose. A guard that suppressed the
    global manifest outright, rather than only during the walk, would satisfy
    the first half alone — and the break it causes (``--global`` no longer
    naming anything) is exactly what the second half pins.

    ``OCX_CEILING_PATH`` bounds the ascent at ``tmp_path`` so the verdict cannot
    depend on what happens to sit above the pytest temp directory on the host.
    """
    home = tmp_path / "home"
    (home / "packages").mkdir(parents=True)
    (home / "ocx.toml").write_text("[tools]\n")
    cwd = home / "packages"
    env = {"OCX_HOME": str(home), "OCX_CEILING_PATH": str(tmp_path)}

    # (1) The walk skips $OCX_HOME's own manifest and keeps ascending, so a
    #     project-tier command from inside the store has no project at all.
    #     `status` is the cheapest observation point: offline and read-only, so
    #     a regression reds here rather than reaching the registry.
    walked = run_in(ocx, cwd, "status", env_extra=env)
    assert walked.returncode == EXIT_USAGE, (
        f"a cwd under $OCX_HOME must be project-free (exit {EXIT_USAGE}), not "
        f"adopt the global toolchain manifest; rc={walked.returncode}, "
        f"stderr={walked.stderr!r}"
    )
    # Exit 64 alone cannot tell the two no-project errors apart: `NoProject`
    # (the walk ran and found nothing) and `NoProjectIn` (an explicit selection
    # named a directory holding no manifest) both classify as 64. Only the walk
    # variant says "no ocx.toml found in" — the other says "no ocx.toml in".
    assert "no ocx.toml found in" in walked.stderr, (
        f"the walk must report NoProject, not some other exit-64 refusal; "
        f"stderr={walked.stderr!r}"
    )

    walked_state = run_in(ocx, cwd, "--offline", "--format", "json", "shell", "state", env_extra=env)
    assert walked_state.returncode == 0, (
        f"`ocx shell state` must succeed; rc={walked_state.returncode}, "
        f"stderr={walked_state.stderr!r}"
    )
    walked_report = json.loads(walked_state.stdout)
    assert walked_report.get("project_dir") is None, (
        f"no project is in effect from a cwd under $OCX_HOME; "
        f"project_dir={walked_report['project_dir']!r}"
    )
    assert Path(walked_report["toolchain_home"]).resolve() == (home / "toolchain").resolve(), (
        f"a project-free shell reports the global toolchain home, not one keyed "
        f"to a project the walk should never have found; "
        f"toolchain_home={walked_report['toolchain_home']!r}"
    )

    # (2) The same manifest stays reachable through the explicit selector.
    #     Asserting the lock is absent first is what makes its presence below
    #     evidence that THIS run wrote it.
    assert not (home / "ocx.lock").exists(), (
        f"nothing has written a lock yet; home contents: "
        f"{sorted(p.name for p in home.iterdir())}"
    )
    locked = run_in(ocx, cwd, "--global", "lock", env_extra=env)
    assert locked.returncode == 0, (
        f"`ocx --global lock` must select $OCX_HOME/ocx.toml from a cwd inside "
        f"$OCX_HOME; rc={locked.returncode}, stderr={locked.stderr!r}"
    )
    assert (home / "ocx.lock").is_file(), (
        f"--global lock must write $OCX_HOME/ocx.lock; home contents: "
        f"{sorted(p.name for p in home.iterdir())}"
    )

    global_state = run_in(ocx, cwd, "-g", "--offline", "--format", "json", "shell", "state", env_extra=env)
    assert global_state.returncode == 0, (
        f"`ocx -g shell state` must succeed; rc={global_state.returncode}, "
        f"stderr={global_state.stderr!r}"
    )
    global_report = json.loads(global_state.stdout)
    # `toolchain_home` is deliberately NOT asserted here: the global scope
    # reports `$OCX_HOME/toolchain` for a project-free invocation too, so it
    # cannot tell "--global selected the manifest" from "--global selected
    # nothing". `project_dir` can — under the selector it is $OCX_HOME itself,
    # and it is null for the plain invocation asserted above.
    assert Path(global_report["project_dir"]).resolve() == home.resolve(), (
        f"--global must put $OCX_HOME itself in effect as the project from a "
        f"cwd below it; project_dir={global_report['project_dir']!r}"
    )


def _project_with_home_manifest(tmp_path: Path) -> tuple[Path, Path, dict[str, str]]:
    home = tmp_path / "home"
    home.mkdir()
    (home / "ocx.toml").write_text("[tools]\n")
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text("[tools]\n")
    return home, project, {"OCX_HOME": str(home), "OCX_CEILING_PATH": str(tmp_path)}


def test_exported_ocx_global_does_not_split_hook_and_shell_allow(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """#678 — with ``OCX_GLOBAL`` exported, ``ocx shell allow`` consents to the project the hook resolves.

    The hook never read ``OCX_GLOBAL``; ``shell allow`` did and answered "is the ocx home".
    The env form is withdrawn, so both agree on the project the walk finds.
    """
    _home, project, env = _project_with_home_manifest(tmp_path)
    env["OCX_GLOBAL"] = "1"

    state = run_in(ocx, project, "--offline", "--format", "json", "shell", "state", env_extra=env)
    assert state.returncode == 0, f"stderr={state.stderr!r}"
    assert Path(json.loads(state.stdout)["project_dir"]).resolve() == project.resolve()

    allowed = run_in(ocx, project, "--offline", "shell", "allow", env_extra=env)
    assert allowed.returncode == 0, (
        f"`shell allow` must consent to the walked project, not refuse the home; "
        f"rc={allowed.returncode}, stderr={allowed.stderr!r}"
    )
    assert str(project.resolve()) in allowed.stderr + allowed.stdout, (
        f"stdout={allowed.stdout!r}, stderr={allowed.stderr!r}"
    )


def test_exported_ocx_project_is_honoured_by_hook_and_shell_allow(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """#678 — an exported ``OCX_PROJECT`` selects the same project for ``shell state`` and ``shell allow``."""
    _home, project, env = _project_with_home_manifest(tmp_path)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    env["OCX_PROJECT"] = str(project / "ocx.toml")

    state = run_in(ocx, elsewhere, "--offline", "--format", "json", "shell", "state", env_extra=env)
    assert state.returncode == 0, f"stderr={state.stderr!r}"
    assert Path(json.loads(state.stdout)["project_dir"]).resolve() == project.resolve()

    allowed = run_in(ocx, elsewhere, "--offline", "shell", "allow", env_extra=env)
    assert allowed.returncode == 0, f"rc={allowed.returncode}, stderr={allowed.stderr!r}"
    assert str(project.resolve()) in allowed.stderr + allowed.stdout
