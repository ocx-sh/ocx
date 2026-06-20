"""End-to-end idempotent move-to-front PATH tests (issue #26).

Drives the *real* ``ocx package env --shell=<s>`` output through an actual
interpreter and asserts the three invariants of the emitted statement:

* **Idempotency** — sourcing the output twice leaves the package bin dir on
  ``PATH`` exactly once (the per-prompt hook re-runs every prompt; #170).
* **Move-to-front** — when the dir is already present mid-``PATH``, re-applying
  moves it to the front (last activation wins for lookup).
* **Self-contained** — the statement is evaluated with no ``ocx`` on ``PATH``;
  it depends on no guard variable and no helper function.

Each shell is skipped when its interpreter is not installed, so a host
``uv run pytest`` stays green while the Docker shell-zoo
(``task test:shells``: bash/zsh/dash/ash/fish/pwsh/nu/elvish) runs the full
matrix.
"""
from __future__ import annotations

import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

import pytest

from uuid import uuid4

from src import OcxRunner, PackageInfo
from src.helpers import make_package


@dataclass(frozen=True)
class ShellRecipe:
    """How to drive one shell: the ``--shell`` value, the interpreter argv, and
    builders for the ``PATH``-seed and ``PATH``-readback snippets."""

    ocx_shell: str
    interpreter: str
    argv: tuple[str, ...]
    seed: Callable[[str], str]
    readback: str

    def script(self, exports: str, seed_path: str, *, twice: bool) -> str:
        body = exports + ("\n" + exports if twice else "")
        return f"{self.seed(seed_path)}\n{body}\n{self.readback}"


SEP = ":"  # POSIX hosts; the shell-zoo image is Linux.


def _posix(ocx_shell: str, interpreter: str) -> ShellRecipe:
    return ShellRecipe(
        ocx_shell=ocx_shell,
        interpreter=interpreter,
        argv=(interpreter, "-c"),
        seed=lambda p: f'export PATH="{p}"',
        readback='printf "%s" "$PATH"',
    )


RECIPES: dict[str, ShellRecipe] = {
    "bash": _posix("bash", "bash"),
    "zsh": _posix("zsh", "zsh"),
    "dash": _posix("dash", "dash"),
    "ksh": _posix("ksh", "ksh"),
    "fish": ShellRecipe(
        ocx_shell="fish",
        interpreter="fish",
        argv=("fish", "-c"),
        seed=lambda p: f'set -gx PATH (string split : "{p}")',
        readback="string join : $PATH",
    ),
    "pwsh": ShellRecipe(
        ocx_shell="pwsh",
        interpreter="pwsh",
        argv=("pwsh", "-NoProfile", "-Command"),
        seed=lambda p: f"$env:PATH='{p}'",
        readback="$env:PATH",
    ),
    "nu": ShellRecipe(
        ocx_shell="nu",
        interpreter="nu",
        argv=("nu", "-c"),
        seed=lambda p: f'$env.PATH = "{p}"',
        readback="$env.PATH | str join (char esep)",
    ),
    "elvish": ShellRecipe(
        ocx_shell="elvish",
        interpreter="elvish",
        argv=("elvish", "-c"),
        seed=lambda p: f'set-env PATH "{p}"',
        readback="echo $E:PATH",
    ),
}


def _bin_dir(ocx: OcxRunner, pkg: PackageInfo) -> str:
    """The package's resolved PATH directory (from the structured env report)."""
    env_json = ocx.json("package", "env", pkg.short)
    path_entry = next(e for e in env_json["entries"] if e["key"] == "PATH")
    # The composed PATH value is the package bin dir (single package, clean env).
    return path_entry["value"].split(SEP)[0]


def _exports(ocx: OcxRunner, pkg: PackageInfo, shell: str) -> str:
    """The eval-safe ``--shell`` output (stdout only)."""
    result = subprocess.run(
        [str(ocx.binary), "package", "env", f"--shell={shell}", pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, f"package env --shell={shell} failed:\n{result.stderr}"
    return result.stdout.strip()


def _run(recipe: ShellRecipe, script: str) -> str:
    """Run ``script`` under the interpreter with a clean env (no ocx on PATH).

    The interpreter binary is resolved to an absolute path so a shell installed
    outside the deliberately minimal subprocess ``PATH`` (e.g. ``pwsh`` under
    ``~/.local/bin``) is still spawnable, while the script-visible ``PATH``
    stays clean to prove the emitted statement is self-contained.
    """
    interpreter = shutil.which(recipe.interpreter) or recipe.interpreter
    proc = subprocess.run(
        [interpreter, *recipe.argv[1:], script],
        capture_output=True,
        text=True,
        env={"PATH": "/usr/bin:/bin:/usr/sbin:/sbin"},
    )
    assert proc.returncode == 0, (
        f"{recipe.interpreter} eval failed (rc={proc.returncode})\n"
        f"script:\n{script}\nstderr:\n{proc.stderr}"
    )
    return proc.stdout.strip()


@pytest.mark.parametrize("shell", list(RECIPES))
def test_shell_emit_is_idempotent(
    ocx: OcxRunner, published_package: PackageInfo, shell: str
):
    """Sourcing ``ocx package env --shell=<s>`` twice keeps the bin dir once."""
    recipe = RECIPES[shell]
    if shutil.which(recipe.interpreter) is None:
        pytest.skip(f"{recipe.interpreter} not installed")

    pkg = published_package
    ocx.plain("package", "install", pkg.short)
    bin_dir = _bin_dir(ocx, pkg)
    exports = _exports(ocx, pkg, recipe.ocx_shell)

    seed = f"/a/bin{SEP}/b/bin"  # bin dir absent initially
    final = _run(recipe, recipe.script(exports, seed, twice=True))
    segments = final.split(SEP)
    assert segments.count(bin_dir) == 1, (
        f"{shell}: bin dir {bin_dir!r} must appear exactly once after a double "
        f"source; got PATH={final!r}"
    )
    assert segments[0] == bin_dir, f"{shell}: bin dir must lead PATH; got {final!r}"


@pytest.mark.parametrize("shell", list(RECIPES))
def test_shell_emit_moves_present_dir_to_front(
    ocx: OcxRunner, published_package: PackageInfo, shell: str
):
    """When the bin dir is already mid-``PATH``, re-applying moves it to front."""
    recipe = RECIPES[shell]
    if shutil.which(recipe.interpreter) is None:
        pytest.skip(f"{recipe.interpreter} not installed")

    pkg = published_package
    ocx.plain("package", "install", pkg.short)
    bin_dir = _bin_dir(ocx, pkg)
    exports = _exports(ocx, pkg, recipe.ocx_shell)

    seed = f"/a/bin{SEP}{bin_dir}{SEP}/b/bin"  # dir present in the middle
    final = _run(recipe, recipe.script(exports, seed, twice=False))
    segments = final.split(SEP)
    assert segments.count(bin_dir) == 1, (
        f"{shell}: re-adding a present dir must not duplicate it; got {final!r}"
    )
    assert segments[0] == bin_dir, (
        f"{shell}: present dir must move to the front; got {final!r}"
    )


def test_bash_emit_does_not_reevaluate_existing_segments(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """Re-sourcing must treat existing ``PATH`` segments as opaque data.

    A pre-existing ``PATH`` segment that *looks* like a command substitution
    must never be executed by the emitted move-to-front statement — the colon
    sentinel runs entirely through parameter expansion, which bash does not
    re-scan for command substitution.
    """
    if shutil.which("bash") is None:
        pytest.skip("bash not installed")

    pkg = published_package
    ocx.plain("package", "install", pkg.short)
    exports = _exports(ocx, pkg, "bash")

    marker = tmp_path / "INJECTED"
    # Single-quote the seed so the hostile segment is *literal data* in PATH;
    # the emitted statement is the only thing that could (wrongly) re-evaluate it.
    seed_literal = f'/a/bin{SEP}$(touch {marker}){SEP}/b/bin'
    script = f"export PATH='{seed_literal}'\n{exports}\nprintf '%s' \"$PATH\""
    _run(RECIPES["bash"], script)
    assert not marker.exists(), (
        "the emitted statement re-evaluated an existing PATH segment (command substitution fired)"
    )


def test_direnv_export_is_idempotent(ocx: OcxRunner, tmp_path: Path):
    """``eval "$(ocx direnv export)"`` twice keeps the toolchain bin dir once.

    direnv re-evaluates ``.envrc`` on every directory change; the emitted bash
    must therefore be self-idempotent (it routes through ``Shell::Bash`` →
    ``export_path``). This locks the guarantee at the ``direnv export`` command
    layer, end to end through a real project + bash.
    """
    if shutil.which("bash") is None:
        pytest.skip("bash not installed")

    label = uuid4().hex[:8]
    repo = f"t_{label}_direnv"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=["tool"])
    fq = f"{ocx.registry}/{repo}:1.0.0"

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\ntool = "{fq}"\n')
    assert _run_in(ocx, project, "lock").returncode == 0
    assert _run_in(ocx, project, "pull").returncode == 0

    export = _run_in(ocx, project, "direnv", "export")
    assert export.returncode == 0, f"direnv export failed:\n{export.stderr}"
    exports = export.stdout.strip()
    assert "export PATH=" in exports, f"expected a PATH export; got:\n{exports}"

    # Source the export block twice in a clean bash (no ocx on PATH) and count
    # how many PATH segments point at the project tool's bin dir.
    script = (
        "export PATH=/usr/bin:/bin:/usr/sbin:/sbin\n"
        f"{exports}\n{exports}\n"
        'printf "%s" "$PATH"'
    )
    final = _run(RECIPES["bash"], script)
    # The toolchain bin dir is the content-addressed package path (it contains
    # the object store's `packages/.../content` segment), not the seed dirs.
    tool_dirs = [seg for seg in final.split(SEP) if "packages" in seg]
    assert tool_dirs, f"tool bin dir missing from PATH after direnv export: {final!r}"
    # Each distinct tool dir must appear exactly once across the two evals.
    for directory in set(tool_dirs):
        assert tool_dirs.count(directory) == 1, (
            f"direnv export is not idempotent: {directory} appears {tool_dirs.count(directory)}x "
            f"in PATH={final!r}"
        )


def _run_in(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run ``ocx`` from ``cwd`` with the runner's isolated env (project commands
    write/read relative to CWD)."""
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
