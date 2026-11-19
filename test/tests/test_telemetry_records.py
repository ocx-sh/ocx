# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Package, process and CI fields of the usage telemetry, read on the wire.

Every assertion reads what a capture OTLP collector received (``otlp_collector``),
so it proves what leaves the machine rather than what ocx meant to build.
``OcxRunner`` builds its environment from scratch, so no ambient ``CI``,
``GITHUB_*`` or ``OTEL_*`` reaches the child. Setup commands (publishing test
packages) run unarmed: only the command under test carries the arming env.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.otlp_fixture import OtlpCollector
from src.runner import OcxRunner, PackageInfo
from src.telemetry_helpers import armed, package_with_dependency

pytestmark = pytest.mark.command(
    "install", "package_pull", "uninstall", "pull", "env", "update", "about", "exec"
)

PACKAGE = "sh.ocx.package"
COMMAND = "sh.ocx.command"
EXIT_NOT_FOUND = 79
EXIT_DATA = 65

GITHUB_ENV = {"GITHUB_ACTIONS": "true", "GITHUB_RUN_ID": "11", "GITHUB_JOB": "build"}
GITLAB_ENV = {
    "GITLAB_CI": "true",
    "CI_PIPELINE_ID": "21",
    "CI_JOB_ID": "22",
    "CI_JOB_NAME": "test",
}


def run_armed(
    ocx: OcxRunner,
    collector: OtlpCollector,
    *args: str,
    env: dict[str, str] | None = None,
    **kwargs: Any,
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
    """Run one armed command and return it with the single export it sent."""
    before = len(collector.exports)
    proc = ocx.run(*args, env_overrides=armed(collector, **(env or {})), **kwargs)
    exports = collector.exports
    assert len(exports) == before + 1, (
        f"expected one export for {args}, got {len(exports) - before}"
    )
    return proc, exports[-1]


def package_records(export: dict[str, Any]) -> list[dict[str, Any]]:
    return [r["attributes"] for r in export["records"] if r["event_name"] == PACKAGE]


def command_record(export: dict[str, Any]) -> dict[str, Any]:
    (record,) = [
        r["attributes"] for r in export["records"] if r["event_name"] == COMMAND
    ]
    return record


def by_role(records: list[dict[str, Any]], role: str) -> dict[str, Any]:
    (record,) = [r for r in records if r["sh.ocx.role"] == role]
    return record


def without_arch(purl: str) -> str:
    return re.sub(r"([?&])arch=[^&]*&?", r"\1", purl).rstrip("?&")


# --------------------------------------------------------------------------- #
# package records
# --------------------------------------------------------------------------- #


def test_install_reports_root_and_dependency_records(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    root, _dep = package_with_dependency(ocx, unique_repo, tmp_path)

    _, export = run_armed(ocx, otlp_collector, "package", "install", root.short, env={})

    records = package_records(export)
    assert len(records) == 2, records
    top = by_role(records, "root")
    assert top["sh.ocx.package.action"] == "installed"
    assert top["sh.ocx.package.store_hit"] is False
    assert top["sh.ocx.package.download.size"] > 0
    for key in ("resolve", "download", "extract"):
        assert top[f"sh.ocx.package.{key}.duration"] >= 0, key
    leaf = by_role(records, "dependency")
    assert leaf["sh.ocx.package.action"] == "pulled"
    assert top["sh.ocx.package"] != leaf["sh.ocx.package"]


def test_reinstall_reports_store_hit_without_fetch_fields(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    root, _dep = package_with_dependency(ocx, unique_repo, tmp_path)
    ocx.run("package", "install", root.short)

    _, export = run_armed(ocx, otlp_collector, "package", "install", root.short)

    top = by_role(package_records(export), "root")
    assert top["sh.ocx.package.action"] == "installed"
    assert top["sh.ocx.package.store_hit"] is True
    assert "sh.ocx.package.resolve.duration" in top
    for absent in (
        "sh.ocx.package.download.size",
        "sh.ocx.package.download.duration",
        "sh.ocx.package.extract.duration",
    ):
        assert absent not in top, absent


def test_package_pull_reports_pulled(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    _, export = run_armed(
        ocx, otlp_collector, "package", "pull", published_package.short
    )

    (record,) = package_records(export)
    assert record["sh.ocx.package.action"] == "pulled"
    assert record["sh.ocx.role"] == "root"
    assert record["sh.ocx.package.store_hit"] is False


def test_uninstall_reports_one_removed_record_matching_the_install_purl(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    _, installed = run_armed(
        ocx, otlp_collector, "package", "install", published_package.short
    )
    (install_record,) = package_records(installed)
    assert (
        "arch=" in install_record["sh.ocx.package"]
    )  # control: the install purl carries a platform

    _, removed = run_armed(
        ocx, otlp_collector, "package", "uninstall", published_package.short
    )

    (record,) = package_records(removed)
    assert record["sh.ocx.package.action"] == "removed"
    assert record["sh.ocx.role"] == "root"
    assert "arch=" not in record["sh.ocx.package"]  # a removal carries no platform
    assert without_arch(record["sh.ocx.package"]) == without_arch(
        install_record["sh.ocx.package"]
    )
    for absent in (
        "sh.ocx.package.store_hit",
        "sh.ocx.package.download.size",
        "sh.ocx.package.resolve.duration",
    ):
        assert absent not in record, absent


def _project_with_tools(ocx: OcxRunner, tmp_path: Path, count: int) -> Path:
    project = tmp_path / "proj"
    project.mkdir()
    lines = ["[tools]"]
    for index in range(count):
        pkg = make_package(ocx, f"t_{uuid4().hex[:8]}_proj{index}", "1.0.0", tmp_path)
        lines.append(f'tool{index} = "{pkg.fq}"')
    (project / "ocx.toml").write_text("\n".join(lines) + "\n")
    ocx.run("--project", str(project), "lock", "--no-pull")
    return project


def test_project_pull_reports_one_pulled_record_per_lock_entry(
    ocx: OcxRunner, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    project = _project_with_tools(ocx, tmp_path, 2)

    _, export = run_armed(ocx, otlp_collector, "--project", str(project), "pull")

    records = package_records(export)
    assert len(records) == 2, records
    assert {r["sh.ocx.package.action"] for r in records} == {"pulled"}
    assert len({r["sh.ocx.package"] for r in records}) == 2


def test_cached_project_env_reports_no_package_records(
    ocx: OcxRunner, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    project = _project_with_tools(ocx, tmp_path, 1)
    ocx.run("--project", str(project), "pull")

    _, export = run_armed(ocx, otlp_collector, "--project", str(project), "env")

    assert (
        command_record(export)["sh.ocx.command.name"] == "env"
    )  # control: the export exists
    assert package_records(export) == []


# --------------------------------------------------------------------------- #
# process and CI fields
# --------------------------------------------------------------------------- #


def test_resource_carries_the_spawned_pid_and_the_parent_pid(
    ocx: OcxRunner, otlp_collector: OtlpCollector
) -> None:
    env = {**ocx.env, **armed(otlp_collector)}
    proc = subprocess.Popen(
        [str(ocx.binary), "about"],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    assert proc.wait(timeout=60) == 0

    (export,) = otlp_collector.exports
    resource = export["resource"]
    assert resource["process.pid"] == proc.pid
    if sys.platform == "win32":
        assert "process.parent.pid" not in resource
    else:
        assert resource["process.parent.pid"] == os.getpid()


@pytest.mark.parametrize(
    ("ci_env", "provider", "run_id", "task_name", "task_run_id"),
    [
        (GITHUB_ENV, "github", "11", "build", None),
        (GITLAB_ENV, "gitlab", "21", "test", "22"),
        ({"CI": "true"}, "other", None, None, None),
    ],
    ids=["github", "gitlab", "ci-only"],
)
def test_ci_provider_and_ids_follow_the_environment(
    ocx: OcxRunner,
    otlp_collector: OtlpCollector,
    ci_env: dict[str, str],
    provider: str,
    run_id: str | None,
    task_name: str | None,
    task_run_id: str | None,
) -> None:
    _, export = run_armed(ocx, otlp_collector, "about", env=ci_env)

    resource = export["resource"]
    assert resource["sh.ocx.ci.provider"] == provider
    assert resource.get("cicd.pipeline.run.id") == run_id
    assert resource.get("cicd.pipeline.task.name") == task_name
    assert resource.get("cicd.pipeline.task.run.id") == task_run_id


def test_no_ci_environment_means_no_ci_keys(
    ocx: OcxRunner, otlp_collector: OtlpCollector
) -> None:
    _, export = run_armed(ocx, otlp_collector, "about")

    keys = set(export["resource"])
    assert "service.name" in keys  # control: a real resource was read
    assert not {k for k in keys if k == "sh.ocx.ci.provider" or k.startswith("cicd.")}


def test_arming_adds_no_variable_to_a_child_environment(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    dump = [
        "--",
        sys.executable,
        "-c",
        "import json, os; print(json.dumps(dict(os.environ)))",
    ]
    ocx.run("package", "install", published_package.short)

    unarmed = json.loads(
        ocx.run("package", "exec", published_package.short, *dump, format=None).stdout
    )
    injected = armed(otlp_collector)
    armed_child = json.loads(
        ocx.run(
            "package",
            "exec",
            published_package.short,
            *dump,
            format=None,
            env_overrides=injected,
        ).stdout
    )

    assert unarmed  # control: the child printed an environment
    assert {k: v for k, v in armed_child.items() if k not in injected} == unarmed
    assert otlp_collector.exports == []  # exec never exports


# --------------------------------------------------------------------------- #
# operator overrides
# --------------------------------------------------------------------------- #


def test_otel_resource_attributes_override_derived_ci_ids(
    ocx: OcxRunner, otlp_collector: OtlpCollector
) -> None:
    _, derived = run_armed(ocx, otlp_collector, "about", env=GITHUB_ENV)
    assert derived["resource"]["cicd.pipeline.run.id"] == "11"  # control: derived value

    _, export = run_armed(
        ocx,
        otlp_collector,
        "about",
        env={
            **GITHUB_ENV,
            "OTEL_RESOURCE_ATTRIBUTES": "cicd.pipeline.run.id=override,team=a%2Cb",
        },
    )

    resource = export["resource"]
    assert resource["cicd.pipeline.run.id"] == "override"
    assert resource["team"] == "a,b"
    assert resource["cicd.pipeline.task.name"] == "build"  # untouched keys stay derived


def test_config_resource_attributes_and_service_name_win_over_the_environment(
    ocx: OcxRunner, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    config = tmp_path / "telemetry.toml"
    config.write_text(
        '[telemetry]\nservice_name = "ocx-ci"\nresource_attributes = { team = "c" }\n'
    )

    _, export = run_armed(
        ocx,
        otlp_collector,
        "--config",
        str(config),
        "about",
        env={
            **GITHUB_ENV,
            "OTEL_RESOURCE_ATTRIBUTES": "team=a%2Cb",
            "OTEL_SERVICE_NAME": "x",
        },
    )

    resource = export["resource"]
    assert resource["team"] == "c"
    assert resource["service.name"] == "ocx-ci"


# --------------------------------------------------------------------------- #
# the command record on failure
# --------------------------------------------------------------------------- #


def test_failed_command_records_exit_code_and_error_type(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    missing = f"{ocx.registry}/{published_package.repo}:9.9.9"

    proc, export = run_armed(
        ocx, otlp_collector, "package", "install", missing, check=False
    )

    assert proc.returncode == EXIT_NOT_FOUND, proc.stderr
    detail = json.loads(proc.stdout)["error"]["detail"]
    record = command_record(export)
    assert record["process.exit.code"] == EXIT_NOT_FOUND
    assert record["error.type"] == detail


def test_non_zero_ok_result_records_exit_code_without_error_type(
    ocx: OcxRunner, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    repo = f"t_{uuid4().hex[:8]}_drift"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=True)
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\nmover = "{ocx.registry}/{repo}:latest"\n'
    )
    ocx.run("--project", str(project), "lock", "--no-pull")
    make_package(
        ocx, repo, "2.0.0", tmp_path, cascade=True
    )  # `latest` moves: the check reports drift

    proc, export = run_armed(
        ocx, otlp_collector, "--project", str(project), "update", "--check", check=False
    )

    assert proc.returncode == EXIT_DATA, proc.stderr
    record = command_record(export)
    assert record["process.exit.code"] == EXIT_DATA
    assert "error.type" not in record
