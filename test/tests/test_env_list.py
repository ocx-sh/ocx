# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance coverage for the `list` env modifier type (#277, W7).

The publish gate now ACCEPTS list-typed metadata (separator REQUIRED on the
wire), so every fixture here goes through the normal `ocx package create` /
`ocx package push` product path via `make_package()` — no raw-push bypass,
except the one test that specifically proves the wire gate rejects metadata
missing the required field.

See ``plan_env_list_type.md`` contract rows W-3/W-4/W-7/W-8/W-9/W-10/W-11 and
the "User Experience Scenarios" table for the behaviors pinned below.

Every custom ``env`` entry passed to ``make_package``/``make_package_with_entrypoints``
needs an explicit ``"visibility": "public"`` — the wire default is
``private``, which the composer's consumer (non-``--self``) view excludes; see
`subsystem-tests.md` "Default env visibility in tests".
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package, make_package_with_entrypoints
from src.runner import OcxRunner, current_platform

EXIT_USAGE = 64  # UsageError (sysexits EX_USAGE)
EXIT_DATA_ERR = 65  # DataError (sysexits EX_DATAERR)
EXIT_SUCCESS = 0


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _list_entry(key: str, value: str, separator: str | None = " ") -> dict:
    """A metadata ``env`` array entry declaring a public `list`-typed var.

    ``separator=None`` omits the field entirely — used only by the wire-gate
    test, which must NOT go through ``make_package`` (that call would itself
    reject the metadata before there was anything to bundle).
    """
    entry: dict = {"key": key, "type": "list", "value": value, "visibility": "public"}
    if separator is not None:
        entry["separator"] = separator
    return entry


def _dumped_value(dump: str, key: str) -> str | None:
    """Return the value of a ``KEY=value`` line in an ``env``-dumped block."""
    prefix = f"{key}="
    for line in dump.splitlines():
        if line.startswith(prefix):
            return line[len(prefix) :]
    return None


def _run_in(
    ocx: OcxRunner, cwd: Path, *args: str, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    """Runs ocx with the process ``cwd`` set to a project directory.

    ``OcxRunner`` has no ``cwd`` parameter — project-tier tests reach for the
    raw subprocess call directly instead (house pattern, see
    ``test_metadata_forward_compat.py::_run_in``).
    """
    return subprocess.run(
        [str(ocx.binary), *args], cwd=cwd, capture_output=True, text=True,
        env=env if env is not None else ocx.env, check=False,
    )


def _make_toolchain_project_with_list_var(
    ocx: OcxRunner, tmp_path: Path, label: str, project_env_body: str
) -> Path:
    """Publish a tool declaring a `,`-separated `GODEBUG` list var, bind it in
    a fresh project, add ``project_env_body`` as the project `[env]` table,
    then `lock` + `pull`. Returns the project directory.
    """
    repo = f"t_{uuid4().hex[:8]}_{label}"
    make_package(
        ocx, repo, "1.0.0", tmp_path, cascade=False, bins=["tool"],
        env=[_list_entry("GODEBUG", "gctrace=1", ",")],
    )
    fq = f"{ocx.registry}/{repo}:1.0.0"

    project = tmp_path / f"proj_{label}"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\ntool = "{fq}"\n\n[env]\n{project_env_body}')

    assert _run_in(ocx, project, "lock").returncode == EXIT_SUCCESS
    assert _run_in(ocx, project, "pull").returncode == EXIT_SUCCESS
    return project


# ---------------------------------------------------------------------------
# Scenario 1 — compose end-to-end (OCI tier) + idempotence
# ---------------------------------------------------------------------------


def test_package_exec_list_var_appends_after_ambient_and_is_idempotent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A package's `list` env var is appended AFTER whatever ambient value the
    invoking process already carries — `ocx package exec` builds its child env
    from `Env::new()` (full inherit), not `Env::clean()`, so the fold sees the
    real ambient. Feeding the fold's own output back as the new ambient
    reproduces it byte-for-byte: the end-to-end shape of `append_unique`'s
    idempotence (`f . f = f`).
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, env=[_list_entry("JDK_JAVA_OPTIONS", "-Xmx2g", " ")]
    )

    def _exec_dump(ambient: str) -> str:
        env = dict(ocx.env)
        env["JDK_JAVA_OPTIONS"] = ambient
        result = subprocess.run(
            [str(ocx.binary), "package", "exec", pkg.short, "--", "env"],
            capture_output=True, text=True, env=env, check=False,
        )
        assert result.returncode == EXIT_SUCCESS, result.stderr
        value = _dumped_value(result.stdout, "JDK_JAVA_OPTIONS")
        assert value is not None, f"JDK_JAVA_OPTIONS missing from dumped env:\n{result.stdout}"
        return value

    first = _exec_dump("-Xss4m")
    assert first == "-Xss4m -Xmx2g", (
        f"the package's contribution must be appended AFTER the ambient value; got {first!r}"
    )

    second = _exec_dump(first)
    assert second == first, (
        f"running the same composition twice (feeding the fold's own output back "
        f"as ambient) must reproduce it byte-for-byte; first={first!r} second={second!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 2 — two packages contribute to the same list var
# ---------------------------------------------------------------------------


def test_two_packages_contribute_to_the_same_list_var_later_composed_last(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Two packages declaring the same `list` var both land in the composed
    value: `ocx package exec <a> <b>` composes `a` then `b`, so `b`'s
    contribution lands at the back.
    """
    pkg_a = make_package(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, env=[_list_entry("JDK_JAVA_OPTIONS", "-Xmx2g", " ")]
    )
    pkg_b = make_package(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, env=[_list_entry("JDK_JAVA_OPTIONS", "-ea", " ")]
    )

    result = subprocess.run(
        [str(ocx.binary), "package", "exec", pkg_a.short, pkg_b.short, "--", "env"],
        capture_output=True, text=True, env=ocx.env, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _dumped_value(result.stdout, "JDK_JAVA_OPTIONS") == "-Xmx2g -ea", (
        f"both packages' contributions must be present, later-composed (pkg_b) last; "
        f"stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# Scenario 3 — launcher re-entry: comma list survives OCX_ENV byte-identical
# ---------------------------------------------------------------------------


def test_env_flag_list_contributions_with_separator_survive_launcher_reentry(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A comma-separated `list` `--env` override reaches a tool THROUGH a
    generated entrypoint launcher byte-identical, including its separator
    (W-7): the launcher re-enters `ocx launcher exec`, which rebuilds its env
    from scratch and relies on the `OCX_ENV` forwarded envelope to recover a
    `--env` override that is not part of the package's own declared metadata.

    Mirrors `test_env.py::test_package_exec_env_flag_survives_generated_entrypoint_launcher`,
    substituting a `list` override (with its separator) for that test's
    `constant` one.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path,
        entrypoints={"showenv": {"command": "env"}},
        env=[{"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"}],
    )
    ocx.plain("package", "install", pkg.short)

    result = subprocess.run(
        [
            str(ocx.binary), "package", "exec",
            "--env", "GODEBUG:list:,=gctrace=1",
            "--env", "GODEBUG:list:,=madvdontneed=1",
            pkg.short, "--", "showenv",
        ],
        capture_output=True, text=True, env=ocx.env, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _dumped_value(result.stdout, "GODEBUG") == "gctrace=1,madvdontneed=1", (
        f"both list overrides must survive the launcher hop, joined by their "
        f"declared comma separator, not silently defaulted to a space; "
        f"stdout:\n{result.stdout}"
    )


def test_direct_launcher_exec_rejects_conflicting_list_separators(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A launcher invoked DIRECTLY (no `ocx run` / `package exec` parent)
    composes its own env from scratch, so it has to settle the same per-key
    separator agreement its callers do.

    Without the reconcile inside `ocx launcher exec`, one package declaring two
    `list` vars for one key with different separators would exit 65 through
    `ocx package exec` and fold with a silently-chosen separator through its own
    generated launcher — the same package, two answers.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
        env=[
            _list_entry("GODEBUG", "gctrace=1", ","),
            _list_entry("GODEBUG", "madvdontneed=1", ";"),
        ],
    )
    ocx.plain("package", "install", pkg.short)

    which = ocx.json("package", "which", pkg.short)
    pkg_root = which[pkg.short]["path"]
    result = ocx.run("launcher", "exec", str(pkg_root), "--", "hello", format=None, check=False)

    assert result.returncode == EXIT_DATA_ERR, (
        f"a direct launcher invocation must refuse conflicting separators like every other "
        f"compose site; got rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    expected = 'conflicting separators "," and ";"'
    assert expected in result.stderr, result.stderr


# ---------------------------------------------------------------------------
# Scenario 4 — per-key separator agreement end-to-end (W-11)
# ---------------------------------------------------------------------------


def test_project_env_list_entry_without_a_separator_inherits_the_package_separator(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A project `[env]` `list` entry that omits `separator` inherits the one a
    package already established for the same key — the composed value stays
    comma-joined and parseable by `GODEBUG`'s consumer, rather than silently
    falling back to the space default.

    Bonus (W-10): `ocx status`'s raw view of the SAME project declaration
    shows the type verbatim with no invented separator — status reports
    `ocx.toml` as written; the comma above is composition, which status does
    not run.
    """
    project = _make_toolchain_project_with_list_var(
        ocx, tmp_path, "inherit", 'GODEBUG = { type = "list", value = "madvdontneed=1" }\n'
    )

    result = _run_in(ocx, project, "run", "--", "env")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _dumped_value(result.stdout, "GODEBUG") == "gctrace=1,madvdontneed=1", (
        f"the project's entry must inherit the package's comma separator; stdout:\n{result.stdout}"
    )

    status = json.loads(_run_in(ocx, project, "--format", "json", "status").stdout)
    godebug = status["groups"]["default"]["env"]["GODEBUG"]
    assert godebug == {"type": "list", "value": "madvdontneed=1"}, godebug


def test_env_flag_conflicting_separator_with_package_established_one_exits_data_error(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A `--env` override that declares an explicit separator conflicting with
    the one a package already established for the same key fails closed (65),
    naming both separators — silently picking either would corrupt the value
    for whichever contributor lost.
    """
    project = _make_toolchain_project_with_list_var(ocx, tmp_path, "conflict", "")

    result = _run_in(ocx, project, "run", "--env", "GODEBUG:list:;=extra", "--", "env")
    assert result.returncode == EXIT_DATA_ERR, (
        f"expected exit {EXIT_DATA_ERR} (DataError); got {result.returncode}\nstderr:\n{result.stderr}"
    )
    expected = (
        'env var \'GODEBUG\' is contributed as a list with conflicting separators '
        '"," and ";"; every contributor to one key must agree'
    )
    assert expected in result.stderr, f"stderr must name both separators; got:\n{result.stderr}"


def test_package_create_rejects_list_metadata_missing_separator(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Package metadata is the wire: a `list` env var MUST declare `separator`
    there (no human is present to be told which one ocx assumed). `ocx
    package create` refuses it at `ValidMetadata`, naming the field, rather
    than a raw serde missing-field error.
    """
    content = tmp_path / "content"
    (content / "bin").mkdir(parents=True)
    (content / "bin" / "app").write_text("#!/bin/sh\necho app\n")

    metadata_path = tmp_path / "metadata.json"
    metadata_path.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "env": [_list_entry("GODEBUG", "gctrace=1", separator=None)],
            }
        )
    )

    bundle = tmp_path / "bundle.tar.xz"
    result = subprocess.run(
        [
            str(ocx.binary), "package", "create",
            "-m", str(metadata_path), "-o", str(bundle), "-p", current_platform(), str(content),
        ],
        capture_output=True, text=True, env=ocx.env, check=False,
    )
    assert result.returncode == EXIT_DATA_ERR, (
        f"expected exit {EXIT_DATA_ERR} (DataError); got {result.returncode}\nstderr:\n{result.stderr}"
    )
    assert "env var 'GODEBUG' omits `separator`, which is required for list entries" in result.stderr, (
        f"stderr must name the offending field; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# Scenario 5 — JSON shape (W-10)
# ---------------------------------------------------------------------------


def test_package_env_json_list_entry_carries_type_and_separator(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx package env`'s JSON `entries` array tags a `list` entry with
    `"type":"list"` and its `"separator"`; a `path` entry in the same
    response carries no `separator` key at all.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        env=[
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin", "visibility": "public"},
            _list_entry("GODEBUG", "gctrace=1", ","),
        ],
    )

    data = ocx.json("package", "env", pkg.short)
    by_key = {e["key"]: e for e in data["entries"]}

    assert by_key["GODEBUG"]["type"] == "list"
    assert by_key["GODEBUG"]["separator"] == ","
    assert "separator" not in by_key["PATH"], f"a path entry must never carry a separator key; got {by_key['PATH']}"


# ---------------------------------------------------------------------------
# Scenario 6 — CI export appends to the runner's ambient value
# ---------------------------------------------------------------------------


def test_ci_github_list_var_appends_to_ambient_in_github_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--ci=github` folds a `list` var's contribution onto the CI runner's
    OWN ambient value and writes ONE line to `$GITHUB_ENV` — the
    append-direction sibling of the `path`/`$GITHUB_PATH` contract already
    covered by `test_ci_export.py`.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, env=[_list_entry("GODEBUG", "gctrace=1", ",")])

    github_path = tmp_path / "github_path"
    github_env = tmp_path / "github_env"
    github_path.write_text("")
    github_env.write_text("")

    env = dict(ocx.env)
    env["GITHUB_ACTIONS"] = "true"
    env["GITHUB_PATH"] = str(github_path)
    env["GITHUB_ENV"] = str(github_env)
    env["GODEBUG"] = "madvdontneed=1"

    result = subprocess.run(
        [str(ocx.binary), "package", "env", pkg.short, "--ci=github"],
        capture_output=True, text=True, env=env, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr

    lines = [ln for ln in github_env.read_text().splitlines() if ln.startswith("GODEBUG=")]
    assert lines == ["GODEBUG=madvdontneed=1,gctrace=1"], (
        f"the runner's ambient value must lead, the package's contribution appended "
        f"after it with the declared comma separator; got: {lines}"
    )


# ---------------------------------------------------------------------------
# Scenario 7 — shell export idempotence (executable proof)
# ---------------------------------------------------------------------------


def test_shell_bash_list_export_is_idempotent_after_double_eval(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx package env --shell=bash`'s `list` export line is eval-safe:
    seeding an ambient value and sourcing the emitted statement TWICE leaves
    the variable byte-stable after the second eval — the executable
    idempotence proof for `Shell::export_list`, mirroring
    `test_path_idempotency.py`'s PATH coverage for the move-to-front fold.
    """
    if shutil.which("bash") is None:
        pytest.skip("bash not installed")

    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, env=[_list_entry("GODEBUG", "gctrace=1", ",")])

    exported = subprocess.run(
        [str(ocx.binary), "package", "env", "--shell=bash", pkg.short],
        capture_output=True, text=True, env=ocx.env, check=False,
    )
    assert exported.returncode == EXIT_SUCCESS, exported.stderr
    exports = exported.stdout.strip()
    assert exports, "expected at least one export line"

    script = f'export GODEBUG="madvdontneed=1"\n{exports}\n{exports}\nprintf "%s" "$GODEBUG"'
    result = subprocess.run(
        ["bash", "-c", script],
        capture_output=True, text=True, env={"PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert result.stdout == "madvdontneed=1,gctrace=1", (
        f"a second eval of the same export line must not grow or reorder the value; got {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Scenario 8 — post-agreement edge rejection
# ---------------------------------------------------------------------------


def test_env_flag_list_value_edged_by_the_inherited_separator_exits_data_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A `--env` list value edged by the separator it INHERITS (no explicit
    `:SEP` on the flag itself) is refused post-agreement, not silently
    accepted with an ambiguous fold: the flag defers its own edge check until
    compose time settles which separator applies.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, env=[_list_entry("GODEBUG", "gctrace=1", ",")])

    result = ocx.run("package", "env", pkg.short, "--env", "GODEBUG:list=,edge", check=False, format=None)
    assert result.returncode == EXIT_DATA_ERR, (
        f"expected exit {EXIT_DATA_ERR} (DataError); got {result.returncode}\nstderr:\n{result.stderr}"
    )
    expected = 'env var \'GODEBUG\' has a list value starting or ending with its separator ",": ",edge"'
    assert expected in result.stderr, result.stderr
