# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for a Docker credential helper installed through ocx (#623).

Such a helper is a toolchain trampoline that re-enters `ocx exec`. When the outer
ocx asks it for credentials while staging a package, the inner ocx waits on the
staging lock the outer one holds. The zots are anonymous, so the helper proves it
ran by appending the server it was asked about to `$OCX_TEST_HELPER_LOG`. Every
run is bounded and killed as a process group: a regression must fail one case,
not hang the suite.
"""

from __future__ import annotations

import json
import os
import re
import signal
import stat
import subprocess
import sys
import time
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package, write_ocx_toml
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo
from src.toolchain_fixtures import shell_bin, toolchain_home

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="the credential helper is a POSIX shell script",
)

EXIT_SUCCESS = 0

#: Bounds one ocx run. The un-fixed code stalls for 30 s per side of the lock,
#: so 20 s is below the first lock timeout and far above a healthy run.
TIMEOUT = 20

#: The name a helper must have for ``credHelpers: {host: "test"}`` to find it.
HELPER_BIN = "docker-credential-test"

#: Answers ``get`` with a credential and logs the server it was asked about.
#: Reads stdin first: the helper protocol delivers the server URL there.
HELPER_SCRIPT = """#!/bin/sh
case "$1" in
get)
    server=$(cat)
    printf '%s\\n' "$server" >> "$OCX_TEST_HELPER_LOG"
    printf '{"ServerURL":"%s","Username":"u","Secret":"s"}\\n' "$server"
    ;;
list) echo '{}' ;;
*) cat > /dev/null ;;
esac
"""


# ---------------------------------------------------------------------------
# Running things
# ---------------------------------------------------------------------------


class Ran:
    """The outcome of one bounded ocx run."""

    def __init__(self, returncode: int | None, stdout: str, stderr: str, elapsed: float):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr
        self.elapsed = elapsed

    @property
    def timed_out(self) -> bool:
        return self.returncode is None


def run_bounded(
    ocx: OcxRunner, cwd: Path, *args: str, env: dict[str, str], timeout: float = TIMEOUT
) -> Ran:
    """Run ``ocx`` in its own process group; kill the whole group on timeout.

    ``subprocess.run(timeout=)`` kills only the direct child, and then waits on
    the output pipes — which the stalled helper and inner ocx still hold.
    """
    started = time.monotonic()
    proc = subprocess.Popen(
        [str(ocx.binary), *args],
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
        returncode: int | None = proc.returncode
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        stdout, stderr = proc.communicate()
        returncode = None
    return Ran(returncode, stdout, stderr, time.monotonic() - started)


def describe(ran: Ran) -> str:
    verdict = f"TIMED OUT after {ran.elapsed:.1f}s" if ran.timed_out else f"rc={ran.returncode}"
    return f"{verdict}\nstdout: {ran.stdout}\nstderr: {ran.stderr}"


def run_ok(ocx: OcxRunner, cwd: Path, *args: str, insecure: str) -> None:
    """A setup step: runs with no credential helper armed and must succeed."""
    env = {**ocx.env, "OCX_INSECURE_REGISTRIES": insecure}
    ran = run_bounded(ocx, cwd, *args, env=env, timeout=120)
    assert ran.returncode == EXIT_SUCCESS, f"`ocx {' '.join(args)}` setup failed: {describe(ran)}"


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def slug(registry: str) -> str:
    """``localhost:5003`` -> ``localhost_5003`` (``StringExt::to_slug``)."""
    return re.sub(r"[^a-zA-Z0-9]", "_", registry)


def publish_helper(ocx: OcxRunner, tmp_path: Path, label: str) -> PackageInfo:
    """The credential helper as an ocx package exposing :data:`HELPER_BIN`."""
    return make_package(
        ocx,
        f"t_{label}_credhelper",
        "1.0.0",
        tmp_path,
        cascade=False,
        bins=[HELPER_BIN],
        bin_scripts={HELPER_BIN: HELPER_SCRIPT},
    )


def publish_dependency(
    ocx: OcxRunner, target_registry: str, tmp_path: Path, label: str
) -> dict:
    """Publish a dependency on ``target_registry``; return its metadata entry."""
    package = make_package(ocx, f"t_{label}_dep", "1.0.0", tmp_path, cascade=False)
    ocx.run(
        "package",
        "copy",
        "--to",
        target_registry,
        package.short,
        env_overlay={"OCX_INSECURE_REGISTRIES": both_insecure(ocx, target_registry)},
    )
    digest = fetch_platform_manifest_digest(target_registry, package.repo, package.tag)
    return {"identifier": f"{target_registry}/{package.repo}:{package.tag}@{digest}"}


def publish_dependent(
    ocx: OcxRunner, target_registry: str, dependency: dict, tmp_path: Path, label: str, tag: str
) -> PackageInfo:
    """A package depending on a package that lives on another registry."""
    return make_package(
        ocx,
        f"t_{label}_dependent",
        tag,
        tmp_path,
        bins=[f"dependent{label}"],
        dependencies=[dependency],
        push_env={"OCX_INSECURE_REGISTRIES": both_insecure(ocx, target_registry)},
    )


def both_insecure(ocx: OcxRunner, target_registry: str) -> str:
    """Both zots speak plain HTTP; the runner's own variable names only one."""
    return f"{ocx.registry},{target_registry}"


def arm_credential_helper(tmp_path: Path, host: str, name: str = "docker-config") -> Path:
    """A ``DOCKER_CONFIG`` directory sending ``host`` to the ``test`` helper."""
    directory = tmp_path / name
    directory.mkdir()
    (directory / "config.json").write_text(json.dumps({"credHelpers": {host: "test"}}))
    return directory


def armed_env(
    ocx: OcxRunner,
    target_registry: str,
    docker_config: Path,
    helper_log: Path,
    path_prefix: Path,
    **extra: str,
) -> dict[str, str]:
    """The environment of an outer ocx that will meet the credential helper."""
    return {
        **ocx.env,
        "DOCKER_CONFIG": str(docker_config),
        "OCX_INSECURE_REGISTRIES": both_insecure(ocx, target_registry),
        "OCX_TEST_HELPER_LOG": str(helper_log),
        "PATH": f"{path_prefix}{os.pathsep}{ocx.env['PATH']}",
        **extra,
    }


def helper_calls(helper_log: Path) -> list[str]:
    return helper_log.read_text().splitlines() if helper_log.exists() else []


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


def test_update_and_pull_do_not_deadlock_on_a_credential_helper_installed_through_ocx(
    ocx: OcxRunner, target_registry: str, tmp_path: Path
) -> None:
    """#623: the helper's trampoline re-enters ocx while the outer ocx holds a staging lock.

    The default group holds the helper and a package ``P``. ``P`` 2.0.0 depends
    on a package on a registry configured with ``credHelpers``, so updating to
    it asks the helper — through its trampoline — for credentials in the middle
    of staging ``P``. The inner ``ocx exec`` must not wait on the lock the outer
    one holds.

    RED on the un-fixed code: the update runs into :data:`TIMEOUT`.
    """
    label = uuid4().hex[:8]
    helper = publish_helper(ocx, tmp_path, label)
    dependency = publish_dependency(ocx, target_registry, tmp_path, label)
    # 1.0.0 carries no dependency: the project starts without the pin the update introduces.
    make_package(ocx, f"t_{label}_dependent", "1.0.0", tmp_path, bins=[f"dependent{label}"])

    project = tmp_path / f"proj-{label}"
    project.mkdir()
    write_ocx_toml(
        project,
        "[tools]\n"
        f'helper = "{helper.fq}"\n'
        f'dependent = "{ocx.registry}/t_{label}_dependent:latest"\n',
    )
    run_ok(ocx, project, "lock", insecure=both_insecure(ocx, target_registry))
    run_ok(ocx, project, "pull", insecure=both_insecure(ocx, target_registry))
    trampoline_dir = shell_bin(toolchain_home(project))
    assert (trampoline_dir / HELPER_BIN).exists(), "setup: the helper's trampoline must be rendered"

    publish_dependent(ocx, target_registry, dependency, tmp_path, label, "2.0.0")

    helper_log = tmp_path / "helper.log"
    env = armed_env(
        ocx,
        target_registry,
        arm_credential_helper(tmp_path, target_registry),
        helper_log,
        trampoline_dir,
    )

    updated = run_bounded(ocx, project, "update", env=env)
    assert not updated.timed_out, f"`ocx update` deadlocked on its own credential helper: {describe(updated)}"
    assert updated.returncode == EXIT_SUCCESS, f"`ocx update` failed: {describe(updated)}"
    assert helper_calls(helper_log) == [target_registry], (
        "the helper must be asked exactly once, for the one registry configured for it; "
        f"got {helper_calls(helper_log)}"
    )

    pulled = run_bounded(ocx, project, "pull", env=env)
    assert not pulled.timed_out, f"`ocx pull` deadlocked on its own credential helper: {describe(pulled)}"
    assert pulled.returncode == EXIT_SUCCESS, f"`ocx pull` failed: {describe(pulled)}"
    assert len(helper_calls(helper_log)) <= 2, (
        f"at most one helper call per registry per run; got {helper_calls(helper_log)}"
    )


def test_a_credential_helper_that_is_not_installed_fails_fast_with_a_message(
    ocx: OcxRunner, target_registry: str, tmp_path: Path
) -> None:
    """The bootstrap case: fetching the helper itself would need the helper.

    The helper's trampoline is on ``PATH`` but its package is absent from the
    ``OCX_HOME`` the pull runs in. The pull must give up on the helper quickly,
    say why, and carry on anonymously (the registries here are anonymous).

    RED on the un-fixed code: the inner ocx installs the helper itself, so the
    pull is silent about it.
    """
    label = uuid4().hex[:8]
    helper = publish_helper(ocx, tmp_path, label)
    dependency = publish_dependency(ocx, target_registry, tmp_path, label)
    dependent = publish_dependent(ocx, target_registry, dependency, tmp_path, label, "1.0.0")

    helper_project = tmp_path / f"helper-proj-{label}"
    helper_project.mkdir()
    write_ocx_toml(helper_project, f'[tools]\nhelper = "{helper.fq}"\n')
    run_ok(ocx, helper_project, "lock", insecure=both_insecure(ocx, target_registry))
    run_ok(ocx, helper_project, "pull", insecure=both_insecure(ocx, target_registry))
    trampoline_dir = shell_bin(toolchain_home(helper_project))
    assert (trampoline_dir / HELPER_BIN).exists(), "setup: the helper's trampoline must be rendered"

    project = tmp_path / f"proj-{label}"
    project.mkdir()
    write_ocx_toml(project, f'[tools]\ndependent = "{dependent.fq}"\n')
    run_ok(ocx, project, "lock", insecure=both_insecure(ocx, target_registry))

    empty_home = tmp_path / f"home-{label}"
    empty_home.mkdir()
    helper_log = tmp_path / "helper.log"
    env = armed_env(
        ocx,
        target_registry,
        arm_credential_helper(tmp_path, target_registry),
        helper_log,
        trampoline_dir,
        OCX_HOME=str(empty_home),
    )

    pulled = run_bounded(ocx, project, "pull", env=env)

    assert not pulled.timed_out, f"`ocx pull` hung on a missing credential helper: {describe(pulled)}"
    assert pulled.returncode == EXIT_SUCCESS, f"anonymous access still works: {describe(pulled)}"
    assert pulled.elapsed < 10, f"a missing helper must fail fast; took {pulled.elapsed:.1f}s"
    assert "not installed" in pulled.stderr, f"the pull must say the helper is not installed: {describe(pulled)}"


@pytest.mark.parametrize(
    "override",
    [
        pytest.param({"TYPE": "token", "TOKEN": "dummy"}, id="token"),
        pytest.param({"TYPE": "anonymous"}, id="anonymous"),
    ],
)
def test_ocx_auth_variables_win_over_a_credential_helper(
    ocx: OcxRunner,
    target_registry: str,
    tmp_path: Path,
    override: dict[str, str],
) -> None:
    """``OCX_AUTH_<registry>_*`` decides the credentials; the helper is never asked.

    The control run, without the variables, shows the helper is reachable and
    asked — otherwise an empty log would hold in every state.
    """
    label = uuid4().hex[:8]
    dependency = publish_dependency(ocx, target_registry, tmp_path, label)
    reference = dependency["identifier"].split("@")[0]

    helper_dir = tmp_path / "helper-bin"
    helper_dir.mkdir()
    helper = helper_dir / HELPER_BIN
    helper.write_text(HELPER_SCRIPT)
    helper.chmod(helper.stat().st_mode | stat.S_IEXEC)
    docker_config = arm_credential_helper(tmp_path, target_registry)

    def pull(home_name: str, log_name: str, **extra: str) -> tuple[Ran, list[str]]:
        home = tmp_path / home_name
        home.mkdir()
        log = tmp_path / log_name
        env = armed_env(
            ocx, target_registry, docker_config, log, helper_dir, OCX_HOME=str(home), **extra
        )
        return run_bounded(ocx, tmp_path, "package", "pull", reference, env=env), helper_calls(log)

    control, control_calls = pull("home-control", "control.log")
    assert control.returncode == EXIT_SUCCESS, f"control pull failed: {describe(control)}"
    assert control_calls, "control: with no OCX_AUTH variable the helper must be asked"

    variables = {f"OCX_AUTH_{slug(target_registry)}_{name}": value for name, value in override.items()}
    overridden, overridden_calls = pull("home-override", "override.log", **variables)

    assert overridden.returncode == EXIT_SUCCESS, f"pull with {sorted(variables)} failed: {describe(overridden)}"
    assert overridden_calls == [], f"an OCX_AUTH variable must keep the helper unasked; got {overridden_calls}"
