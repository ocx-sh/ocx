"""Tests for CI env export via ``--ci`` on ``ocx env`` / ``ocx package env``.

The deleted ``ocx ci`` command does NOT return — CI export is realized as the
``--ci[=PROVIDER]`` flag (handshake §6, ADR ``adr_ci_env_export_flag.md``):

- ``--ci=github`` appends to the ``$GITHUB_PATH`` / ``$GITHUB_ENV`` files.
- ``--ci=gitlab`` writes JSON-lines ``{"name","value"}`` to ``--export-file``
  or stdout.
- bare ``--ci`` autodetects from ``$GITHUB_ACTIONS`` / ``$GITLAB_CI``.
- ``--ci`` ⟂ ``--shell``; ``--export-file`` rejected for ``--ci=github``.

CI runner variables are set directly in the child env here — the ``__OCX_``
test-seam prefix does not apply, these are real provider variables. The
``OcxRunner`` env is minimal (it does not inherit ``GITHUB_ACTIONS`` etc. from
the host), so the "no CI env" cases are deterministic even when this suite runs
inside GitHub Actions.
"""
from __future__ import annotations

import json
import re as _re_ci
import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo

# ocx maps clap usage errors → EX_USAGE (64); ci::error::MissingEnv → EX_CONFIG (78).
EXIT_USAGE = 64
EXIT_CONFIG = 78


def _expected_entries(ocx: OcxRunner, pkg: PackageInfo) -> dict[str, dict[str, str]]:
    """Resolve the package env once (JSON) so sinks can be checked against it.

    Returns ``{key: {"type": ..., "value": ...}}`` in the default consumer view.
    """
    data = ocx.json("package", "env", pkg.short)
    return {e["key"]: {"type": e["type"], "value": e["value"]} for e in data["entries"]}


def _run_env(
    ocx: OcxRunner,
    *args: str,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx package env <args>`` (no ``--format``) and return the result."""
    return subprocess.run(
        [str(ocx.binary), "package", "env", *args],
        capture_output=True,
        text=True,
        env=env if env is not None else ocx.env,
    )


# ── GitHub Actions: two-file sink ──────────────────────────────────────────


def test_ci_github_writes_env_and_path_files(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``package env --ci=github`` appends to $GITHUB_ENV and $GITHUB_PATH."""
    expected = _expected_entries(ocx, published_package)

    github_path = tmp_path / "github_path"
    github_env = tmp_path / "github_env"
    github_path.write_text("")
    github_env.write_text("")

    env = dict(ocx.env)
    env["GITHUB_ACTIONS"] = "true"
    env["GITHUB_PATH"] = str(github_path)
    env["GITHUB_ENV"] = str(github_env)

    result = _run_env(ocx, published_package.short, "--ci=github", env=env)
    assert result.returncode == 0, (
        f"--ci=github must succeed inside GitHub Actions; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    # PATH-type entries land in $GITHUB_PATH, one directory per line.
    path_lines = [ln for ln in github_path.read_text().splitlines() if ln.strip()]
    # Constants land in $GITHUB_ENV as KEY=VALUE.
    env_vars: dict[str, str] = {}
    for line in github_env.read_text().splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            env_vars[key] = value

    for key, info in expected.items():
        if info["type"] == "path" and key == "PATH":
            assert info["value"] in path_lines, (
                f"PATH entry {info['value']!r} missing from $GITHUB_PATH\n"
                f"got: {path_lines}"
            )
        elif info["type"] == "constant":
            assert env_vars.get(key) == info["value"], (
                f"constant {key}={info['value']!r} missing from $GITHUB_ENV\n"
                f"got: {env_vars}"
            )


def test_ci_github_rejects_export_file(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``--ci=github --export-file`` → exit 64 (GitHub infers its sink)."""
    env = dict(ocx.env)
    env["GITHUB_ACTIONS"] = "true"
    env["GITHUB_PATH"] = str(tmp_path / "gh_path")
    env["GITHUB_ENV"] = str(tmp_path / "gh_env")

    result = _run_env(
        ocx,
        published_package.short,
        "--ci=github",
        f"--export-file={tmp_path / 'nope.env'}",
        env=env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"--ci=github + --export-file must exit {EXIT_USAGE}; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


def test_ci_github_outside_actions_is_config_error(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """Explicit ``--ci=github`` with no $GITHUB_ENV/$GITHUB_PATH → exit 78.

    The global-lenient contract covers resolution failures only; an explicit CI
    channel that cannot find its sink is a config error.
    """
    env = dict(ocx.env)
    env.pop("GITHUB_ACTIONS", None)
    env.pop("GITHUB_ENV", None)
    env.pop("GITHUB_PATH", None)

    result = _run_env(ocx, published_package.short, "--ci=github", env=env)
    assert result.returncode == EXIT_CONFIG, (
        f"explicit --ci=github without sink files must exit {EXIT_CONFIG}; "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ── GitLab CI/CD: JSON-lines ───────────────────────────────────────────────


def test_ci_gitlab_export_file_json_lines(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``package env --ci=gitlab --export-file`` writes JSON-lines."""
    expected = _expected_entries(ocx, published_package)
    export = tmp_path / "export.env"

    result = _run_env(
        ocx,
        published_package.short,
        "--ci=gitlab",
        f"--export-file={export}",
    )
    assert result.returncode == 0, (
        f"--ci=gitlab --export-file must succeed; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    lines = [json.loads(ln) for ln in export.read_text().splitlines() if ln.strip()]
    by_name = {obj["name"]: obj["value"] for obj in lines}
    # Each JSON object has exactly the two pinned keys.
    for obj in lines:
        assert set(obj.keys()) == {"name", "value"}, f"unexpected keys in {obj}"

    for key, info in expected.items():
        assert key in by_name, f"{key} missing from GitLab export\ngot: {by_name}"
        if info["type"] == "path" and key == "PATH":
            # GitLab has no path channel: PATH is flattened (package value
            # prepended onto the existing process PATH).
            assert by_name[key].startswith(info["value"]), (
                f"PATH value must start with {info['value']!r}; got {by_name[key]!r}"
            )
        elif info["type"] == "constant":
            assert by_name[key] == info["value"], (
                f"{key} value mismatch: want {info['value']!r}, got {by_name[key]!r}"
            )


def test_ci_gitlab_stdout_default(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """``package env --ci=gitlab`` (no --export-file) writes JSON-lines to stdout."""
    expected = _expected_entries(ocx, published_package)

    result = _run_env(ocx, published_package.short, "--ci=gitlab")
    assert result.returncode == 0, (
        f"--ci=gitlab (stdout) must succeed; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    lines = [json.loads(ln) for ln in result.stdout.splitlines() if ln.strip()]
    by_name = {obj["name"]: obj["value"] for obj in lines}
    assert by_name, f"expected JSON-lines on stdout; got:\n{result.stdout!r}"
    for key, info in expected.items():
        assert key in by_name, f"{key} missing from stdout export\ngot: {by_name}"
        if info["type"] == "constant":
            assert by_name[key] == info["value"]


# ── Usage errors ───────────────────────────────────────────────────────────


def test_ci_conflicts_with_shell(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """``--ci`` and ``--shell`` together → exit 64 (clap conflict)."""
    result = _run_env(ocx, published_package.short, "--ci=gitlab", "--shell=bash")
    assert result.returncode == EXIT_USAGE, (
        f"--ci + --shell must exit {EXIT_USAGE}; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


def test_ci_export_file_requires_ci(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``--export-file`` without ``--ci`` → exit 64 (clap requires)."""
    result = _run_env(
        ocx, published_package.short, f"--export-file={tmp_path / 'x.env'}"
    )
    assert result.returncode == EXIT_USAGE, (
        f"--export-file without --ci must exit {EXIT_USAGE}; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


def test_ci_bare_autodetect_no_ci_env_is_usage_error(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """Bare ``--ci`` with no CI env vars → exit 64 (cannot autodetect)."""
    env = dict(ocx.env)
    env.pop("GITHUB_ACTIONS", None)
    env.pop("GITLAB_CI", None)

    result = _run_env(ocx, published_package.short, "--ci", env=env)
    assert result.returncode == EXIT_USAGE, (
        f"bare --ci with no CI env must exit {EXIT_USAGE}; got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


def test_ci_command_still_removed(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """The ``ocx ci`` command stays removed — capability is a flag, not a command."""
    result = subprocess.run(
        [str(ocx.binary), "ci", "export", published_package.short],
        cwd=str(tmp_path),
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx ci must exit {EXIT_USAGE} (removed command); got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


# ── Toolchain-tier ``ocx env --ci`` (root tier, NOT ``package env``) ──────────


def _make_toolchain_project(
    ocx: OcxRunner,
    tmp_path: Path,
    label: str,
    bin_name: str = "tool",
) -> Path:
    """Publish a package, create a locked + pulled project, return project_dir.

    Reuses the same three-step setup established in ``test_toolchain_env.py``:
    make_package → write ocx.toml → ocx lock → ocx pull.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_{label}"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False, bins=[bin_name])
    fq = f"{ocx.registry}/{repo}:1.0.0"

    project = tmp_path / f"proj_{label}"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\n{bin_name} = "{fq}"\n')

    lock_result = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_result.returncode == 0, (
        f"ocx lock failed during project setup; rc={lock_result.returncode}\n"
        f"stderr:\n{lock_result.stderr}"
    )

    pull_result = subprocess.run(
        [str(ocx.binary), "pull"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert pull_result.returncode == 0, (
        f"ocx pull failed during project setup; rc={pull_result.returncode}\n"
        f"stderr:\n{pull_result.stderr}"
    )

    return project


def test_toolchain_env_ci_gitlab_export_file_json_lines(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Root toolchain-tier ``ocx env --ci=gitlab --export-file`` writes JSON-lines.

    Covers the resolution prologue that is unique to the toolchain tier:
    ``load_project_with_lock`` → ``compose_tool_set`` → ``find_or_install_all``
    → ``resolve_env`` → ``export_ci`` (shared sink with ``ocx package env``).

    Asserts the export file contains valid JSON-lines ``{"name","value"}`` for
    the composed toolchain environment and that every object has exactly those
    two keys.
    """
    project = _make_toolchain_project(ocx, tmp_path, "tc_gitlab_export")
    export = tmp_path / "tc_export.env"

    result = subprocess.run(
        [str(ocx.binary), "env", "--ci=gitlab", f"--export-file={export}"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, (
        f"ocx env --ci=gitlab --export-file must succeed for a locked project; "
        f"got rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    assert export.exists(), (
        f"--export-file must be created by ocx env --ci=gitlab; path: {export}"
    )

    raw = export.read_text()
    lines = [ln for ln in raw.splitlines() if ln.strip()]
    assert lines, (
        f"export file must contain at least one JSON-line; got:\n{raw!r}"
    )

    for raw_line in lines:
        obj = json.loads(raw_line)
        assert set(obj.keys()) == {"name", "value"}, (
            f"each JSON object must have exactly {{name, value}} keys; got {obj}"
        )
        assert isinstance(obj["name"], str) and obj["name"], (
            f"'name' must be a non-empty string; got {obj!r}"
        )
        assert isinstance(obj["value"], str), (
            f"'value' must be a string; got {obj!r}"
        )


def test_toolchain_env_ci_conflicts_with_shell(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Root toolchain-tier ``ocx env --ci=gitlab --shell=bash`` → exit 64.

    Locks the shared ``conventions.rs`` contract (``--ci`` ⟂ ``--shell``,
    clap ``conflicts_with``) at the toolchain tier, not only at the OCI tier.
    """
    project = _make_toolchain_project(ocx, tmp_path, "tc_ci_shell_conflict")

    result = subprocess.run(
        [str(ocx.binary), "env", "--ci=gitlab", "--shell=bash"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx env --ci=gitlab --shell=bash must exit {EXIT_USAGE} "
        f"(--ci conflicts with --shell); got {result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# ``ocx env --ci=gitlab`` resolves the host leaf for the export
# ---------------------------------------------------------------------------

_LEAF_RE_CI = _re_ci.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def test_toolchain_env_ci_gitlab_resolves_host_leaf(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Root toolchain-tier ``ocx env --ci=gitlab`` resolves the tool's host-leaf
    digest from ``ocx.lock``'s ``[tool.platforms]`` table and emits the
    correct environment to the CI export file.

    Scenario: publish, ``ocx lock``, ``ocx pull``, then ``ocx env
    --ci=gitlab --export-file=<f>``.  Assert:
    1. The project lock carries ``[tool.platforms]`` with at least one leaf
       digest, no ``pinned =`` line.
    2. The CI export file contains at least one JSON-line with ``name``+``value``
       keys (proves the leaf was resolved and the env was computed).

    The comment at test_ci_export.py:168 ("two pinned keys") refers to JSON
    object keys ``{"name", "value"}`` in GitLab export lines — NOT to the lock
    ``pinned`` field.  That comment is unrelated to the lock format and is
    intentionally left unmodified.
    """
    project = _make_toolchain_project(ocx, tmp_path, "tc_ci_v3_leaf", bin_name="citool")

    lock_text = (project / "ocx.lock").read_text()
    assert "[tool.platforms]" in lock_text, (
        "project lock must carry a [tool.platforms] table"
    )
    leaf_digests = _LEAF_RE_CI.findall(lock_text)
    assert leaf_digests, "lock must record at least one leaf digest"
    assert "pinned =" not in lock_text, (
        "lock must not carry a legacy `pinned` line"
    )

    export = tmp_path / "ci_v3_export.env"
    result = subprocess.run(
        [str(ocx.binary), "env", "--ci=gitlab", f"--export-file={export}"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, (
        f"ocx env --ci=gitlab must exit 0 (host-leaf resolved); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert export.exists(), (
        "ocx env --ci=gitlab must create the --export-file"
    )
    lines = [ln for ln in export.read_text().splitlines() if ln.strip()]
    assert lines, (
        "CI export file must contain at least one JSON-line (env resolved)"
    )
    for raw_line in lines:
        obj = json.loads(raw_line)
        assert set(obj.keys()) == {"name", "value"}, (
            f"each JSON object must have exactly {{name, value}} keys; got {obj}"
        )
