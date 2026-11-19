# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for telemetry opt-outs, destination precedence and the managed tier.

Specs: system_design_otel_telemetry.md § 3.1 to § 3.3 and the CI behaviour. Every "sends nothing"
assertion follows a control on the same fixture that must have sent, so a
capture server that never listened cannot make the negative half pass.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest

from src.helpers import push_managed_config
from src.otlp_fixture import LOGS_PATH, OtlpCollector, serve_collector
from src.runner import OcxRunner, PackageInfo
from src.telemetry_helpers import armed, write_config

pytestmark = pytest.mark.command("install", "package_pull")

COMMAND_EVENT = "sh.ocx.command"


@pytest.fixture()
def collector_b() -> Iterator[OtlpCollector]:
    with serve_collector("ok") as collector:
        yield collector
    collector.assert_no_decode_failures()


def _config_armed(ocx: OcxRunner, collector: OtlpCollector) -> dict[str, str]:
    write_config(
        ocx,
        f'[telemetry]\nenabled = true\nendpoint = "{collector.endpoint}"\n',
    )
    return {}


def _install(
    ocx: OcxRunner, pkg: PackageInfo, env: dict[str, str], *flags: str
) -> subprocess.CompletedProcess[str]:
    return ocx.run(*flags, "package", "install", pkg.short, env_overrides=env)


def _warnings(result: subprocess.CompletedProcess[str]) -> list[str]:
    return [line for line in result.stderr.splitlines() if line.startswith("warning:")]


# --- each opt-out wins -----------------------------------------------

OPT_OUTS = [
    pytest.param({"DO_NOT_TRACK": "1"}, (), id="do-not-track"),
    pytest.param({"OTEL_SDK_DISABLED": "true"}, (), id="otel-sdk-disabled"),
    pytest.param({"OTEL_LOGS_EXPORTER": "none"}, (), id="otel-logs-exporter-none"),
    pytest.param({}, ("--offline",), id="offline-flag"),
    pytest.param({"OCX_OFFLINE": "1"}, (), id="offline-env"),
]


@pytest.mark.parametrize("arming", ["env", "config"])
@pytest.mark.parametrize(("opt_out", "flags"), OPT_OUTS)
def test_opt_out_beats_arming(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    arming: str,
    opt_out: dict[str, str],
    flags: tuple[str, ...],
) -> None:
    """each opt-out silences a run armed by the switch or by `[telemetry] enabled`."""
    # Warm the store so the offline row installs the same package.
    _install(ocx, published_package, {})
    env = (
        armed(otlp_collector) if arming == "env" else _config_armed(ocx, otlp_collector)
    )
    assert otlp_collector.requests == []

    _install(ocx, published_package, env)
    assert len(otlp_collector.log_posts) == 1, "control: the armed run must send"

    result = _install(ocx, published_package, {**env, **opt_out}, *flags)
    assert result.returncode == 0
    assert len(otlp_collector.requests) == 1, "the opt-out run sent a request"


@pytest.mark.parametrize("value", ["0", "maybe"])
def test_switch_off_or_invalid_beats_config_arming(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    value: str,
) -> None:
    """`OCX_TELEMETRY=0` and an unrecognized value both silence `[telemetry] enabled = true`."""
    _config_armed(ocx, otlp_collector)
    _install(ocx, published_package, {})
    assert len(otlp_collector.log_posts) == 1, "control: config alone must send"

    result = _install(ocx, published_package, {"OCX_TELEMETRY": value})

    assert result.returncode == 0
    assert len(otlp_collector.requests) == 1
    warnings = _warnings(result)
    if value == "maybe":
        assert len(warnings) == 1, result.stderr
        assert "OCX_TELEMETRY" in warnings[0]
        assert "maybe" not in result.stderr
    else:
        assert warnings == []


# --- destination precedence ------------------------------------------


def test_a_later_endpoint_layer_drops_headers_of_earlier_layers(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    tmp_path: Path,
) -> None:
    """user-config headers never ride along to an endpoint another layer chose."""
    headers = '[telemetry]\nheaders = { authorization = "Bearer u" }\n'
    overlay = tmp_path / "overlay.toml"

    # Control: headers and endpoint from one layer reach the wire.
    write_config(
        ocx,
        headers + f'endpoint = "{otlp_collector.endpoint}"\n',
    )
    overlay.write_text("[telemetry]\nenabled = true\n", encoding="utf-8")
    _install(ocx, published_package, {"OCX_CONFIG": str(overlay)})
    assert otlp_collector.log_posts[0].headers["authorization"] == "Bearer u"

    write_config(ocx, headers)
    overlay.write_text(
        f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n',
        encoding="utf-8",
    )
    _install(ocx, published_package, {"OCX_CONFIG": str(overlay)})

    assert len(otlp_collector.log_posts) == 2
    assert otlp_collector.log_posts[1].headers["authorization"] is None


def test_config_endpoint_beats_otel_endpoint(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    collector_b: OtlpCollector,
) -> None:
    """`[telemetry].endpoint` wins over `OTEL_EXPORTER_OTLP_ENDPOINT`, which serves alone otherwise."""
    otel_b = {"OTEL_EXPORTER_OTLP_ENDPOINT": collector_b.endpoint}
    _config_armed(ocx, otlp_collector)

    _install(ocx, published_package, otel_b)
    assert len(otlp_collector.log_posts) == 1
    assert collector_b.requests == []

    write_config(ocx, "[telemetry]\nenabled = true\n")
    _install(ocx, published_package, otel_b)
    assert len(collector_b.log_posts) == 1
    assert len(otlp_collector.requests) == 1


def test_ambient_otel_headers_do_not_follow_a_config_endpoint(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    collector_b: OtlpCollector,
) -> None:
    """with `[telemetry].endpoint` A selected, `OTEL_EXPORTER_OTLP_HEADERS` reaches nobody."""
    otel = {
        "OTEL_EXPORTER_OTLP_ENDPOINT": collector_b.endpoint,
        "OTEL_EXPORTER_OTLP_HEADERS": "x-otel=1",
    }
    _config_armed(ocx, otlp_collector)
    _install(ocx, published_package, otel)
    assert len(otlp_collector.log_posts) == 1
    assert otlp_collector.log_posts[0].headers["x-otel"] is None

    write_config(ocx, "[telemetry]\nenabled = true\n")
    _install(ocx, published_package, otel)
    assert collector_b.log_posts[0].headers["x-otel"] == "1", (
        "control: the same headers do reach a destination chosen from OTEL_*"
    )


def test_ocx_headers_are_not_sent_to_an_otel_destination(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    collector_b: OtlpCollector,
) -> None:
    """`OCX_TELEMETRY_HEADERS` is dropped for an OTEL destination, with one warning naming both sources."""
    write_config(ocx, "[telemetry]\nenabled = true\n")
    result = _install(
        ocx,
        published_package,
        {
            "OTEL_EXPORTER_OTLP_ENDPOINT": collector_b.endpoint,
            "OCX_TELEMETRY_HEADERS": "x-ocx=1",
        },
    )
    assert len(collector_b.log_posts) == 1
    assert collector_b.log_posts[0].headers["x-ocx"] is None
    warnings = _warnings(result)
    assert len(warnings) == 1, result.stderr
    assert "OCX_TELEMETRY_HEADERS" in warnings[0]
    assert "[telemetry]" in warnings[0]

    _config_armed(ocx, otlp_collector)
    _install(ocx, published_package, {"OCX_TELEMETRY_HEADERS": "x-ocx=1"})
    assert otlp_collector.log_posts[0].headers["x-ocx"] == "1", (
        "control: the same header reaches a `[telemetry].endpoint` destination"
    )


def test_generic_headers_never_reach_a_logs_endpoint(
    ocx: OcxRunner, published_package: PackageInfo, collector_b: OtlpCollector
) -> None:
    """`OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` takes the logs-specific headers, never the generic ones.

    Ignoring `OTEL_EXPORTER_OTLP_HEADERS` is announced once on stderr, by name, never its value.
    """
    secret = "generic-header-value-not-a-secret"
    env = {
        "OCX_TELEMETRY": "1",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": collector_b.url(LOGS_PATH),
        "OTEL_EXPORTER_OTLP_HEADERS": f"x-generic={secret}",
    }
    results = [
        _install(ocx, published_package, env),
        _install(
            ocx,
            published_package,
            {**env, "OTEL_EXPORTER_OTLP_LOGS_HEADERS": "x-logs=l1"},
        ),
    ]

    first, second = collector_b.log_posts
    assert first.headers["x-generic"] is None
    assert second.headers["x-generic"] is None
    assert second.headers["x-logs"] == "l1", "control: logs headers are sent"
    for result in results:
        ignored = [w for w in _warnings(result) if "OTEL_EXPORTER_OTLP_HEADERS" in w]
        assert len(ignored) == 1, result.stderr
        assert "OTEL_EXPORTER_OTLP_LOGS_HEADERS" in ignored[0]
        assert secret not in result.stderr


# --- switch versus file ----------------------------------------------


def test_switch_on_beats_enabled_false(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """`OCX_TELEMETRY=1` arms a file that says `enabled = false`; without it the file stays off."""
    write_config(
        ocx,
        f'[telemetry]\nenabled = false\nendpoint = "{otlp_collector.endpoint}"\n',
    )
    _install(ocx, published_package, {})
    assert otlp_collector.requests == []

    _install(ocx, published_package, {"OCX_TELEMETRY": "1"})
    assert len(otlp_collector.log_posts) == 1


# --- managed payload arms --------------------------------------------


def test_managed_payload_arms_and_carries_headers(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """a managed snapshot arms telemetry and its headers reach the wire; `OCX_NO_CONFIG=1` skips the tier."""
    managed_repo = f"{unique_repo}_managed"
    push_managed_config(
        ocx,
        managed_repo,
        "v1",
        f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n'
        'headers = { authorization = "Bearer t" }\n',
        tmp_path,
    )
    ref = f"{ocx.registry}/{managed_repo}:v1"
    ocx.run("config", "setup", "--managed-config", ref)
    assert otlp_collector.requests == [], "adopting the tier must not send"

    _install(ocx, published_package, {})
    assert len(otlp_collector.log_posts) == 1
    assert otlp_collector.log_posts[0].headers["authorization"] == "Bearer t"

    _install(ocx, published_package, {"OCX_NO_CONFIG": "1"})
    assert len(otlp_collector.requests) == 1, "the managed tier must be skipped"


def test_managed_payload_with_a_bad_value_leaves_telemetry_off(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """a managed `[telemetry]` section that parses but holds a bad value is unusable: off, exit 0."""
    managed_repo = f"{unique_repo}_managed_bad"
    push_managed_config(
        ocx,
        managed_repo,
        "v1",
        f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n'
        'timeout = "fast"\n',
        tmp_path,
    )
    ocx.run("config", "setup", "--managed-config", f"{ocx.registry}/{managed_repo}:v1")
    _install(ocx, published_package, {})
    baseline = _install(ocx, published_package, {})

    result = _install(ocx, published_package, {})

    assert result.returncode == 0, result.stderr
    assert result.stdout == baseline.stdout
    assert otlp_collector.requests == []
    assert "fast" not in result.stderr
    warnings = _warnings(result)
    assert len(warnings) == 1 and "timeout" in warnings[0], result.stderr


# --- unusable section ------------------------------------------------


_ENDPOINT_LINE = 'endpoint = "{endpoint}"\n'


@pytest.mark.parametrize(
    ("section", "switch", "key"),
    [
        pytest.param(
            'enabled = true\n{ep}timeout = "fast"\n',
            False,
            "timeout",
            id="enabled-true",
        ),
        pytest.param(
            'enabled = true\n{ep}timeout = "fast"\n',
            True,
            "timeout",
            id="enabled-true-and-switch",
        ),
        pytest.param('{ep}timeout = "fast"\n', True, "timeout", id="switch-only"),
        pytest.param(
            'enabled = "yes"\n{ep}', False, "enabled", id="invalid-enabled-itself"
        ),
    ],
)
def test_unusable_section_turns_telemetry_off_with_one_warning_when_export_was_asked(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    section: str,
    switch: bool,
    key: str,
) -> None:
    """when the run asked for export, an unusable section silences it, names file and key
    (not the value) in one warning, and changes nothing else."""
    _install(ocx, published_package, {})
    baseline = _install(ocx, published_package, {})
    endpoint = _ENDPOINT_LINE.format(endpoint=otlp_collector.endpoint)

    config = write_config(ocx, "[telemetry]\n" + section.format(ep=endpoint))
    env = {"OCX_TELEMETRY": "1"} if switch else {}
    result = _install(ocx, published_package, env)

    assert result.returncode == 0
    assert result.stdout == baseline.stdout
    assert otlp_collector.requests == []
    warnings = _warnings(result)
    assert len(warnings) == 1, result.stderr
    assert str(config) in warnings[0]
    assert key in warnings[0]
    assert "fast" not in result.stderr and "yes" not in result.stderr

    write_config(ocx, f"[telemetry]\nenabled = true\n{endpoint}")
    _install(ocx, published_package, env)
    assert len(otlp_collector.log_posts) == 1, (
        "control: the same file without the bad value sends"
    )


def test_unusable_section_is_silent_when_export_was_not_asked(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """broken `headers`, no `OCX_TELEMETRY`, no `enabled`: off, exit 0, no warning, no request."""
    _install(ocx, published_package, {})
    baseline = _install(ocx, published_package, {})
    endpoint = _ENDPOINT_LINE.format(endpoint=otlp_collector.endpoint)
    config = write_config(ocx, f'[telemetry]\n{endpoint}headers = "not-a-table"\n')

    result = _install(ocx, published_package, {})

    assert result.returncode == 0
    assert result.stdout == baseline.stdout
    assert _warnings(result) == [], result.stderr
    assert otlp_collector.requests == []

    # Control: the same broken file does warn once the run asks for export.
    asked = _install(ocx, published_package, {"OCX_TELEMETRY": "1"})
    asked_warnings = _warnings(asked)
    assert len(asked_warnings) == 1, asked.stderr
    assert str(config) in asked_warnings[0] and "headers" in asked_warnings[0]
    assert otlp_collector.requests == []

    # Control: with the value fixed and the switch on, the export arrives.
    write_config(ocx, f"[telemetry]\n{endpoint}")
    _install(ocx, published_package, {"OCX_TELEMETRY": "1"})
    assert len(otlp_collector.log_posts) == 1


def test_unusable_section_is_ignored_under_no_config(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """an unlocked user file is not read under `OCX_NO_CONFIG=1`, so its broken section warns nothing."""
    write_config(ocx, '[telemetry]\nenabled = true\ntimeout = "fast"\n')
    env = {**armed(otlp_collector), "OCX_NO_CONFIG": "1"}

    result = _install(ocx, published_package, env)

    assert result.returncode == 0
    assert _warnings(result) == []
    assert len(otlp_collector.log_posts) == 1, "control: the env-armed run sends"


# --- nested ocx ------------------------------------------------------


def _exec_inner(
    ocx: OcxRunner,
    pkg: PackageInfo,
    inner: list[str],
    env: dict[str, str],
    *flags: str,
) -> subprocess.CompletedProcess[str]:
    return ocx.run(
        "package",
        "exec",
        *flags,
        pkg.short,
        "--",
        str(ocx.binary),
        *inner,
        env_overrides=env,
        check=False,
    )


def test_nested_ocx_sends_once_from_the_inner_command(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """`ocx package exec` never sends; the `ocx package pull` it launches does, once."""
    ocx.run("package", "install", published_package.short)

    result = _exec_inner(
        ocx,
        published_package,
        ["package", "pull", published_package.short],
        armed(otlp_collector),
    )

    assert result.returncode == 0, result.stderr
    assert len(otlp_collector.requests) == 1
    (export,) = otlp_collector.exports
    (command,) = [r for r in export["records"] if r["event_name"] == COMMAND_EVENT]
    assert command["attributes"]["sh.ocx.command.name"] == "package pull"


def test_a_child_ocx_under_an_offline_parent_does_not_arm(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """offline reaches the child through the received narrowing, so the child sends nothing."""
    ocx.run("package", "install", published_package.short)
    inner = ["package", "pull", published_package.short]

    control = _exec_inner(ocx, published_package, inner, armed(otlp_collector))
    assert control.returncode == 0, control.stderr
    assert len(otlp_collector.log_posts) == 1, "control: the online child must send"

    result = ocx.run(
        "--offline",
        "package",
        "exec",
        published_package.short,
        "--",
        str(ocx.binary),
        *inner,
        env_overrides=armed(otlp_collector),
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert len(otlp_collector.requests) == 1, "the offline child sent a request"


def test_nested_ocx_under_toolchain_exec_sends_once_and_shares_the_execution_record_pids(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    tmp_path: Path,
) -> None:
    """`ocx exec -- ocx package pull` sends once, from the inner command; on POSIX the exec
    keeps the pid, so the execution record and the export agree on pid and parent pid."""
    pkg = published_package
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\n{pkg.repo} = "{ocx.registry}/{pkg.repo}:{pkg.tag}"\n',
        encoding="utf-8",
    )
    ocx.run("--project", str(project), "lock")
    ocx.run("--project", str(project), "pull")
    sink = tmp_path / "records"
    sink.mkdir()

    proc = subprocess.Popen(
        [
            str(ocx.binary),
            "--project",
            str(project),
            "exec",
            "--records-dir",
            str(sink),
            "--",
            str(ocx.binary),
            "package",
            "pull",
            pkg.short,
        ],
        env={**ocx.env, **armed(otlp_collector)},
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    _, stderr = proc.communicate(timeout=120)
    assert proc.returncode == 0, stderr

    assert len(otlp_collector.requests) == 1, "the outer `ocx exec` must not send"
    (export,) = otlp_collector.exports
    (command,) = [r for r in export["records"] if r["event_name"] == COMMAND_EVENT]
    assert command["attributes"]["sh.ocx.command.name"] == "package pull"

    if sys.platform == "win32":
        return  # ocx spawns rather than execs there: the pids legitimately differ
    # Final records are `{time}-{pid}-{rand}.json`; an in-flight temp is `.tmp*` with no `.json`.
    (record_path,) = sorted(sink.glob("*.json"))
    process = json.loads(record_path.read_text(encoding="utf-8"))["process"]
    assert process["pid"] == proc.pid, "control: the record names the exec'd process"
    assert export["resource"]["process.pid"] == process["pid"]
    assert process["parent"]["pid"] == os.getpid()
    assert export["resource"]["process.parent.pid"] == process["parent"]["pid"]


def test_clean_exec_drops_env_arming_but_not_config_arming(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """`--clean` removes `OCX_TELEMETRY` from the child; a config file the home still names keeps arming."""
    ocx.run("package", "install", published_package.short)
    inner = ["clean", "--dry-run"]

    env_only = _exec_inner(
        ocx, published_package, inner, armed(otlp_collector), "--clean"
    )
    assert env_only.returncode == 0, env_only.stderr
    assert otlp_collector.requests == []

    _config_armed(ocx, otlp_collector)
    config_armed = _exec_inner(ocx, published_package, inner, {}, "--clean")
    assert config_armed.returncode == 0, config_armed.stderr
    assert len(otlp_collector.log_posts) == 1, (
        "control: the inner command ran and reached the collector"
    )


# --- CI --------------------------------------------------------------


def test_export_runs_in_ci(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """`CI=true` and a piped stderr never stop the export."""
    env = {**armed(otlp_collector), "CI": "true", "GITHUB_ACTIONS": "true"}

    _install(ocx, published_package, env)

    assert len(otlp_collector.log_posts) == 1
