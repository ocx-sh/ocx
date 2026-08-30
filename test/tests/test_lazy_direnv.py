# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for scenario S-011 — a deferred tool through `direnv`.

**What this file does not do: it never runs `direnv`.** `direnv` is not in the
project toolchain (`ocx.toml`'s `[tools]`), no CI workflow installs it, and no
test in this suite has ever invoked it. What is exercised here is ocx's entire
half of the contract: `ocx direnv export` emits shell lines, and `direnv`'s job
is to evaluate them in the prompt's shell. These tests evaluate exactly those
lines in a real non-interactive `bash` and then invoke the shimmed name from
that environment.

So a passing run says: *the environment `ocx direnv export` produces composes a
deferred tool onto `PATH`, and invoking the name from it materializes and
execs.* It does not say anything about direnv's own `.envrc` allow-listing,
watch-file re-firing, or export-diff bookkeeping — nothing here observes
direnv, so read no evidence about it into a green.

Related coverage: `test_direnv.py` (the `ocx direnv init` / `export`
subcommands themselves) and `test_lazy_loading.py` (S-001…S-012 minus S-011).
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from src import OcxRunner, PackageInfo
from src.helpers import (
    assert_shim_dir_absent,
    assert_shim_dir_exists,
    make_package,
    write_ocx_toml,
)
from src.shell_eval import run_after_sourcing

EXIT_SUCCESS = 0

PUBLIC_BIN_PATH = [
    {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin", "visibility": "public"}
]


def _run(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args], cwd=cwd, capture_output=True, text=True, env=env, check=False
    )


def _deferred_project(ocx: OcxRunner, tmp_path: Path, pkg: PackageInfo) -> Path:
    """A project whose single tool is deferred, locked against a cold store."""
    project = tmp_path / "project"
    project.mkdir()
    write_ocx_toml(project, f'lazy-mode = "always"\n\n[tools]\nhello = "{pkg.fq}"\n')
    result = _run(ocx, project, "lock", "--no-pull")
    assert result.returncode == EXIT_SUCCESS, f"ocx lock --no-pull failed:\n{result.stderr}"
    return project


def _is_materialized(ocx: OcxRunner, project: Path, pkg: PackageInfo) -> bool:
    """Store probe via `ocx package which` — resolves locally, never installs."""
    result = _run(ocx, project, "--format", "json", "package", "which", pkg.short)
    return result.returncode == EXIT_SUCCESS


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_s011_direnv_export_composes_a_deferred_tool_and_the_first_call_materializes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-011: the exported environment carries the shim `bin/`, and using it works.

    Three claims in the scenario, asserted in order: the export puts the shim
    directory on `PATH` with no content on disk; the first invocation
    materializes and execs; a second invocation is served by the real `bin/`.

    `hash -r` separates the two lookups — bash caches a resolved command path,
    so without it the second call would answer from that cache and prove
    nothing about which directory `PATH` now reaches first.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _deferred_project(ocx, tmp_path, pkg)

    export = _run(ocx, project, "direnv", "export")
    assert export.returncode == EXIT_SUCCESS, (
        f"S-011: `ocx direnv export` must succeed; rc={export.returncode}\nstderr:\n{export.stderr}"
    )
    shim_bin = assert_shim_dir_exists(ocx, pkg.repo, "S-011: direnv export composes the shim tree")
    assert str(shim_bin) in export.stdout, (
        f"S-011: the exported PATH must carry the shim `bin/` {shim_bin}; got:\n{export.stdout}"
    )
    assert not _is_materialized(ocx, project, pkg), (
        "S-011: exporting a deferred tool must not materialize its content"
    )

    # `OCX_BINARY_PIN` is what every ocx-spawned child gets; a shell that
    # sources export lines has to supply it, or the generated launcher falls
    # back to whatever `ocx` the ambient PATH happens to hold.
    shell_env = {**ocx.env, "OCX_BINARY_PIN": str(ocx.binary)}
    result = run_after_sourcing(
        export.stdout,
        "command -v hello\nhello\nhash -r\ncommand -v hello\nhello",
        cwd=project,
        env=shell_env,
    )

    assert result.returncode == EXIT_SUCCESS, (
        f"S-011: invoking the shimmed name from the exported env must work; "
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert result.stdout.count(pkg.marker) == 2, (
        f"S-011: both invocations must run the real binary; marker={pkg.marker!r} "
        f"stdout={result.stdout!r}"
    )
    resolved = [line for line in result.stdout.splitlines() if line.endswith("/hello")]
    assert resolved[0] == str(shim_bin / "hello"), (
        f"S-011: the first lookup must find the shim; got {resolved[0]}"
    )
    assert resolved[1].endswith("/content/bin/hello"), (
        f"S-011: after materialization the same name must be served by the real "
        f"`content/bin/`; got {resolved[1]}"
    )
    assert _is_materialized(ocx, project, pkg), "S-011: the first invocation materialized the tool"


def test_s011_direnv_export_no_pull_omits_a_tool_whose_metadata_is_absent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-011 error case: `--no-pull` warns and omits — the prompt never fails.

    `ocx direnv export` is best-effort by contract: it runs on every `cd`, so a
    tool it cannot compose has to degrade to a comment on stderr and an exit 0,
    never to a failed prompt. The `# ocx:` prefix keeps that line inert if the
    stream is sourced.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _deferred_project(ocx, tmp_path, pkg)

    result = _run(ocx, project, "direnv", "export", "--no-pull")

    assert result.returncode == EXIT_SUCCESS, (
        f"S-011: an uncomposable tool must not fail the prompt; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    assert "hello not installed" in result.stderr, (
        f"S-011: the omitted tool must be named on stderr; got:\n{result.stderr}"
    )
    assert result.stderr.lstrip().startswith("# ocx:"), (
        f"S-011: the diagnostic must stay inert if the stream is sourced; got:\n{result.stderr}"
    )
    assert result.stdout.strip() == "", (
        f"S-011: nothing composed means nothing exported; got:\n{result.stdout}"
    )
    assert_shim_dir_absent(ocx, pkg.repo, "S-011: --no-pull must not fetch the tool's metadata")

    # Pairs the negative above with a positive on the same project: dropping
    # `--no-pull` publishes the shim tree. Without this the absence assertion
    # would hold just as well for an eagerly composed project, and the test
    # would not be about a deferred tool at all.
    assert _run(ocx, project, "direnv", "export").returncode == EXIT_SUCCESS
    assert_shim_dir_exists(ocx, pkg.repo, "S-011: --no-pull was what suppressed the shim")


def test_s011_direnv_export_accepts_the_lazy_mode_flag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """C-021: `ocx direnv export` is the seventh command that takes `--lazy-mode`.

    It is on the list because the `ocx.toml` tiers can defer a tool with no
    flag typed at all — a direnv-composed environment that ignored `lazy-mode`
    would differ from the `ocx env` one for the same project. The flag's top-
    tier precedence is what this asserts: `--lazy-mode never` overrides the
    file's `always` and composes eagerly.
    """
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"], env=PUBLIC_BIN_PATH
    )
    project = _deferred_project(ocx, tmp_path, pkg)

    eager = _run(ocx, project, "direnv", "export", "--lazy-mode", "never")

    assert eager.returncode == EXIT_SUCCESS, (
        f"C-021: `ocx direnv export --lazy-mode never` must be accepted; "
        f"rc={eager.returncode}\nstderr:\n{eager.stderr}"
    )
    assert_shim_dir_absent(ocx, pkg.repo, "C-021: --lazy-mode never composes eagerly, with no shim")
    assert _is_materialized(ocx, project, pkg), (
        "C-021: the CLI tier outranks the toolchain tier, so the tool materialized"
    )

    # The other half of the override, on the same project and in this order so
    # the assertion above ran against a store the flag itself shaped: with no
    # flag the file's `always` applies and the shim tree appears.
    default = _run(ocx, project, "direnv", "export")
    assert default.returncode == EXIT_SUCCESS, f"direnv export failed:\n{default.stderr}"
    assert_shim_dir_exists(ocx, pkg.repo, "C-021: without the flag the toolchain tier defers")


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase (S-010)")
def test_direnv_export_pull_retry_keeps_every_root_in_request_order(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """One missing eager tool must not disturb the deferred tools around it.

    `direnv export` probes offline first and, when anything was omitted, pulls.
    That retry composes only the omitted subset, so its results have to be
    spliced back among the roots the probe already produced — and the roots
    vector carries no slot for an omission, so the two sets are re-interleaved
    by replaying the requests. This is the guard on that splice: the deferred
    tool on either side of the missing eager one must keep its position.

    Deliberately a behaviour-preservation test, not a discriminating one for
    the performance defect it accompanies: passing the whole request set to the
    retry (the previous shape) produced the *same* output, only after re-running
    the deferred half for every tool. What is falsifiable here is the splice —
    an off-by-one in it swaps a shim `bin/` for a package `bin/` on `PATH`.
    """
    first = make_package(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, bins=["aa"], binaries=["aa"], env=PUBLIC_BIN_PATH
    )
    middle = make_package(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, bins=["bb"], binaries=["bb"], env=PUBLIC_BIN_PATH
    )
    last = make_package(
        ocx, f"{unique_repo}_c", "1.0.0", tmp_path, bins=["cc"], binaries=["cc"], env=PUBLIC_BIN_PATH
    )

    project = tmp_path / "project"
    project.mkdir()
    lazy_only = (
        f'[tools]\naa = "{first.fq}"\ncc = "{last.fq}"\n\n'
        f'[package."{first.fq}"]\nlazy-mode = "always"\n\n'
        f'[package."{last.fq}"]\nlazy-mode = "always"\n'
    )
    write_ocx_toml(project, lazy_only)
    assert _run(ocx, project, "lock", "--no-pull").returncode == EXIT_SUCCESS

    # Warm the two deferred tools first, so the mixed export below really is
    # mixed: their metadata is local and the offline probe composes them, while
    # the eager tool added next has never been fetched at all.
    primed = _run(ocx, project, "direnv", "export")
    assert primed.returncode == EXIT_SUCCESS, f"priming export failed:\n{primed.stderr}"
    assert_shim_dir_exists(ocx, first.repo, "the first deferred tool is warm before the mixed export")
    assert_shim_dir_exists(ocx, last.repo, "the last deferred tool is warm before the mixed export")

    write_ocx_toml(
        project,
        f'[tools]\naa = "{first.fq}"\nbb = "{middle.fq}"\ncc = "{last.fq}"\n\n'
        f'[package."{first.fq}"]\nlazy-mode = "always"\n\n'
        f'[package."{middle.fq}"]\nlazy-mode = "never"\n\n'
        f'[package."{last.fq}"]\nlazy-mode = "always"\n',
    )
    assert _run(ocx, project, "lock", "--no-pull").returncode == EXIT_SUCCESS
    assert not _is_materialized(ocx, project, middle), (
        "precondition: only the eager tool in the middle is missing, or the retry "
        "composes everything and the splice is never exercised"
    )

    partial = _run(ocx, project, "direnv", "export")
    assert partial.returncode == EXIT_SUCCESS, (
        f"a partially-warm export must succeed; rc={partial.returncode}\nstderr:\n{partial.stderr}"
    )
    first_bin = assert_shim_dir_exists(ocx, first.repo, "the first deferred tool composed")
    last_bin = assert_shim_dir_exists(ocx, last.repo, "the last deferred tool composed")
    assert _is_materialized(ocx, project, middle), "the eager tool in the middle was pulled by the retry"

    warm = _run(ocx, project, "direnv", "export")
    assert warm.stdout == partial.stdout, (
        "the retry's roots must land in request order, so a fully-warm export is "
        f"byte-identical\npartial:\n{partial.stdout}\nwarm:\n{warm.stdout}"
    )
    for shim_bin in (first_bin, last_bin):
        assert str(shim_bin) in partial.stdout, (
            f"the deferred tool's shim `bin/` {shim_bin} must survive the retry; got:\n{partial.stdout}"
        )
    assert "/content/bin" in partial.stdout, (
        f"the pulled eager tool must contribute its real `bin/`; got:\n{partial.stdout}"
    )
