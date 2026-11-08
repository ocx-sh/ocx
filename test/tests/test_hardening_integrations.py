# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Hardening scenarios for secrets, exporters and integrations around the exec state.

Each test is one scenario; a passing one stays as a regression test. Sentinel values are
dash-joined, never token-shaped. Values are recorded verbatim; only ocx's
four credentials and `OCX_AUTH_*` are never recorded. Shell-hook shells, tmux and Windows cases
live in the other hardening modules.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import textwrap
from pathlib import Path
from uuid import uuid4

import pytest

from src import exec_state
from src.helpers import make_package, resolved_metadata_path
from src.runner import OcxRunner, current_platform
from src.shell_eval import run_after_sourcing

pytestmark = [
    pytest.mark.command("exec", "env", "direnv_export", "shell"),
    pytest.mark.skipif(sys.platform == "win32", reason="POSIX shells and scripts"),
]

CARRIER = "__OCX_ENV_STATE"
SENTINEL = "fixture-secret-value"
PRINT_STATE = ["sh", "-c", f"printenv {CARRIER}"]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run(
    ocx: OcxRunner, cwd: Path, *args: str, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env if env is not None else dict(ocx.env),
        stdin=subprocess.DEVNULL,
        timeout=60,
        check=False,
    )


def _project(ocx: OcxRunner, root: Path, env_block: str) -> Path:
    """An `[env]`-only project, locked (which records the consent the hooks need)."""
    root.mkdir(parents=True, exist_ok=True)
    (root / "ocx.toml").write_text(f"[tools]\n\n[env]\n{env_block}\n", encoding="utf-8")
    locked = _run(ocx, root, "lock")
    assert locked.returncode == 0, locked.stderr
    return root


def _payload(raw: str) -> str:
    """The decoded state as JSON text, so a leak is found wherever it sits in the payload."""
    decoded = exec_state.decode(raw.strip())
    assert isinstance(decoded, dict), f"expected a readable state, got {decoded!r}"
    return json.dumps(decoded)


def _applied_literal(raw: str, key: str) -> str | None:
    """The literal value the decoded state's applied record for `key` holds, or None without a record."""
    decoded = exec_state.decode(raw.strip())
    assert isinstance(decoded, dict), f"expected a readable state, got {decoded!r}"
    for record in decoded["applied"]:
        if record["key"] == key:
            return record["value"]["literal"]
    return None


def _exec_state_of(
    ocx: OcxRunner, cwd: Path, *flags: str, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return _run(ocx, cwd, "exec", *flags, "--", *PRINT_STATE, env=env)


# ---------------------------------------------------------------------------
# Secrets: --env, project [env], URL userinfo
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "key", ["GITHUB_TOKEN", "DB_PASSWORD", "MY_API_KEY", "AWS_SECRET_ACCESS_KEY"]
)
def test_secret_named_env_flag_value_is_recorded_verbatim_in_the_child_state(
    ocx: OcxRunner, tmp_path: Path, key: str
) -> None:
    """`--env KEY=value` reaches the child and its state records the value as composed, whatever the key."""
    project = _project(ocx, tmp_path / "p", 'PLAIN_CONST = "plain-fixture-value"')
    result = _exec_state_of(ocx, project, "--env", f"{key}={SENTINEL}")
    assert result.returncode == 0, result.stderr
    assert _applied_literal(result.stdout, key) == SENTINEL


def test_forwarded_ambient_secret_is_recorded_verbatim_in_the_child_state(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """`--env NAME` forwards an ambient value by name; the state records it as forwarded."""
    project = _project(ocx, tmp_path / "p", 'PLAIN_CONST = "plain-fixture-value"')
    env = {**ocx.env, "DEPLOY_TOKEN": SENTINEL}
    result = _exec_state_of(ocx, project, "--env", "DEPLOY_TOKEN", env=env)
    assert result.returncode == 0, result.stderr
    assert _applied_literal(result.stdout, "DEPLOY_TOKEN") == SENTINEL


@pytest.mark.parametrize(
    "url",
    [
        f"https://user:{SENTINEL}@registry.example/path",
        f"https://{SENTINEL}@registry.example/path",
        f"ssh://git:{SENTINEL}@host.example:2222/repo",
    ],
)
def test_url_userinfo_on_a_plain_key_is_recorded_verbatim(
    ocx: OcxRunner, tmp_path: Path, url: str
) -> None:
    """A value holding a URL with userinfo is recorded as composed, like any other value."""
    project = _project(ocx, tmp_path / "p", 'PLAIN_CONST = "plain-fixture-value"')
    result = _exec_state_of(ocx, project, "--env", f"MIRROR_ENDPOINT={url}")
    assert result.returncode == 0, result.stderr
    assert _applied_literal(result.stdout, "MIRROR_ENDPOINT") == url


def test_url_userinfo_in_a_project_constant_is_recorded_verbatim(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    url = f"https://user:{SENTINEL}@feed.example/simple"
    project = _project(ocx, tmp_path / "p", f'FEED_URL = "{url}"')
    result = _exec_state_of(ocx, project)
    assert result.returncode == 0, result.stderr
    assert _applied_literal(result.stdout, "FEED_URL") == url


@pytest.mark.parametrize(
    "key", ["DEPLOY_PAT", "DB_PWD", "SESSION_JWT", "SLACK_WEBHOOK_URL", "GH_PAT"]
)
def test_less_common_secret_names_are_recorded_verbatim(
    ocx: OcxRunner, tmp_path: Path, key: str
) -> None:
    """No name rule decides what is recorded: these keys record their composed value too."""
    project = _project(ocx, tmp_path / "p", f'{key} = "{SENTINEL}"')
    result = _exec_state_of(ocx, project)
    assert result.returncode == 0, result.stderr
    assert _applied_literal(result.stdout, key) == SENTINEL


@pytest.mark.parametrize("level", ["debug", "trace"])
def test_verbose_logging_never_prints_a_secret_constant(
    ocx: OcxRunner, tmp_path: Path, level: str
) -> None:
    project = _project(ocx, tmp_path / "p", f'GITHUB_TOKEN = "{SENTINEL}"')
    result = _run(
        ocx,
        project,
        "--log-level",
        level,
        "exec",
        "--env",
        f"DB_PASSWORD={SENTINEL}",
        "--",
        "true",
    )
    assert result.returncode == 0, result.stderr
    assert SENTINEL not in result.stderr, "a --log-level run printed a secret value"
    assert SENTINEL not in result.stdout


_CREDENTIALS = (
    "OCX_IDENTITY_TOKEN",
    "OCX_KEY_PASSWORD",
    "OCX_SIGNING_KEY",
    "OCX_ANNOUNCE_GIT_TOKEN",
)


def test_shell_state_report_shows_the_composed_literal_and_no_credential_value(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The child's `shell state` lists a secret-named constant with its value, and never holds ocx's own credentials."""
    credential_value = "fixture-credential-value"
    project = _project(ocx, tmp_path / "p", f'GITHUB_TOKEN = "{SENTINEL}"')
    env = {**ocx.env, **{name: credential_value for name in _CREDENTIALS}}
    result = _run(
        ocx,
        project,
        "exec",
        "--env",
        f"DB_PASSWORD={SENTINEL}",
        "--",
        str(ocx.binary),
        "--offline",
        "--format",
        "json",
        "shell",
        "state",
        env=env,
    )
    assert result.returncode == 0, result.stderr
    assert credential_value not in result.stdout + result.stderr
    applied = {r["key"]: r for r in json.loads(result.stdout)["env_state"]["applied"]}
    assert applied["GITHUB_TOKEN"]["value"] == SENTINEL, applied["GITHUB_TOKEN"]
    assert "withheld" not in applied["GITHUB_TOKEN"], applied["GITHUB_TOKEN"]
    assert applied["DB_PASSWORD"]["value"] == SENTINEL, applied["DB_PASSWORD"]
    assert not set(_CREDENTIALS) & set(applied), sorted(applied)


def test_isolate_removes_a_recorded_secret_named_constant_and_keeps_a_changed_one(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A project constant on `GITHUB_TOKEN` is recorded; `--isolate` removes it while it still holds that value, and keeps a value the user re-exported."""
    project = _project(ocx, tmp_path / "p", 'GITHUB_TOKEN = "tok-sentinel"')
    probe = "printf '%s' \"${GITHUB_TOKEN-unset}\""
    # The inner call names another project, or its explicit exec would compose the constant again.
    other = _project(ocx, tmp_path / "other", 'OTHER_VAR = "other"')
    inner = [
        str(ocx.binary),
        "--offline",
        "--project",
        str(other),
        "exec",
        "--isolate",
        "--",
        "sh",
        "-c",
        probe,
    ]

    untouched = _run(ocx, project, "exec", "--", *inner)
    assert untouched.returncode == 0, untouched.stderr
    assert untouched.stdout == "unset", untouched.stdout

    changed = _run(
        ocx,
        project,
        "exec",
        "--",
        "sh",
        "-c",
        'GITHUB_TOKEN=mine; export GITHUB_TOKEN; exec "$@"',
        "sh",
        *inner,
    )
    assert changed.returncode == 0, changed.stderr
    assert changed.stdout == "mine", changed.stdout


# ---------------------------------------------------------------------------
# Exporters: injection through values
# ---------------------------------------------------------------------------

HOSTILE_VALUES = {
    "single_quote": "it's 'quoted'",
    "command_substitution": "$(touch INJECTION_MARKER)",
    "backticks": "`touch INJECTION_MARKER`",
    "newline": "line1\\nINJECTED_KEY=evil",
    "double_quote_and_dollar": 'say "hi" $HOME ${IFS}',
    "backslash": "back\\\\slash",
}


def _hostile_project(ocx: OcxRunner, tmp_path: Path, value: str) -> Path:
    return _project(
        ocx,
        tmp_path / "p",
        f'HOSTILE_VALUE = "{value.replace(chr(34), chr(92) + chr(34))}"',
    )


@pytest.mark.parametrize("name", sorted(HOSTILE_VALUES))
@pytest.mark.parametrize("command", [("env", "--shell=bash"), ("direnv", "export")])
def test_shell_exporter_neutralises_hostile_values(
    ocx: OcxRunner, tmp_path: Path, name: str, command: tuple[str, ...]
) -> None:
    """Sourcing the export runs no command smuggled in a value and sets the value verbatim."""
    value = HOSTILE_VALUES[name]
    project = _hostile_project(ocx, tmp_path, value)
    exported = _run(ocx, project, *command)
    assert exported.returncode == 0, exported.stderr
    work = tmp_path / "work"
    work.mkdir()
    result = run_after_sourcing(
        exported.stdout,
        'printf "%s" "$HOSTILE_VALUE"; echo; echo "[${INJECTED_KEY-unset}]"',
        work,
        dict(ocx.env),
    )
    assert not (work / "INJECTION_MARKER").exists(), (
        "a value ran a command while the export was sourced"
    )
    assert "[unset]" in result.stdout, "a value defined a second variable"
    expected = json.loads(f'"{value}"') if "\\n" in value or "\\\\" in value else value
    assert result.stdout.split("\n[")[0] == expected, result.stdout


def _parse_github_env(text: str) -> dict[str, str]:
    values: dict[str, str] = {}
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        if "<<" in line.split("=", 1)[0]:
            key, _, delimiter = line.partition("<<")
            body: list[str] = []
            i += 1
            while i < len(lines) and lines[i] != delimiter:
                body.append(lines[i])
                i += 1
            values[key] = "\n".join(body)
        elif "=" in line:
            key, _, value = line.partition("=")
            values[key] = value
        i += 1
    return values


def test_github_env_file_cannot_be_injected_through_a_value(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A value holding a newline and a delimiter-like line stays one variable in `$GITHUB_ENV`."""
    value = "first\\nINJECTED_KEY=evil\\nEOF\\nINJECTED_TWO=evil"
    project = _project(ocx, tmp_path / "p", f'HOSTILE_VALUE = "{value}"')
    path_file, env_file = tmp_path / "gh_path", tmp_path / "gh_env"
    path_file.write_text("")
    env_file.write_text("")
    env = {
        **ocx.env,
        "GITHUB_ACTIONS": "true",
        "GITHUB_PATH": str(path_file),
        "GITHUB_ENV": str(env_file),
    }
    done = _run(ocx, project, "env", "--ci=github", env=env)
    assert done.returncode == 0, done.stderr
    parsed = _parse_github_env(env_file.read_text())
    assert "INJECTED_KEY" not in parsed and "INJECTED_TWO" not in parsed, sorted(parsed)
    assert parsed["HOSTILE_VALUE"] == "first\nINJECTED_KEY=evil\nEOF\nINJECTED_TWO=evil"


def test_github_path_file_takes_one_line_per_entry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A path entry holding a newline cannot add a second `$GITHUB_PATH` line."""

    def export(name: str, value: str) -> tuple[subprocess.CompletedProcess[str], str]:
        project = _project(
            ocx, tmp_path / name, f'PATH = {{ type = "path", value = "{value}" }}'
        )
        path_file, env_file = tmp_path / f"{name}_path", tmp_path / f"{name}_env"
        path_file.write_text("")
        env_file.write_text("")
        env = {
            **ocx.env,
            "GITHUB_ACTIONS": "true",
            "GITHUB_PATH": str(path_file),
            "GITHUB_ENV": str(env_file),
        }
        done = _run(ocx, project, "env", "--ci=github", env=env)
        assert done.returncode == 0, done.stderr
        return done, path_file.read_text()

    # Positive control: a clean entry does land in the file, so an empty file below
    # is the refusal and not a broken exporter.
    _, clean = export("clean", "bin")
    assert [line for line in clean.splitlines() if line.endswith("/bin")], clean

    hostile, written = export("hostile", "bin\\nINJECTED_DIR")
    assert "INJECTED_DIR" not in written, written
    assert "embedded newline" in hostile.stderr, hostile.stderr


def test_exporter_state_entry_records_the_composed_value_verbatim(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The state entry of an exporter holds the value as composed (`--format json env` report)."""
    project = _project(ocx, tmp_path / "p", f'GITHUB_TOKEN = "{SENTINEL}"')
    done = _run(ocx, project, "--format", "json", "env")
    assert done.returncode == 0, done.stderr
    for item in json.loads(done.stdout)["items"]:
        if item["key"] == CARRIER:
            assert _applied_literal(item["value"], "GITHUB_TOKEN") == SENTINEL


def test_direnv_export_stderr_is_free_of_secret_values(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    project = _project(ocx, tmp_path / "p", f'GITHUB_TOKEN = "{SENTINEL}"')
    done = _run(ocx, project, "--log-level", "debug", "direnv", "export")
    assert done.returncode == 0, done.stderr
    assert SENTINEL not in done.stderr


# ---------------------------------------------------------------------------
# Plugin dispatch
# ---------------------------------------------------------------------------

NAMES_PLUGIN = textwrap.dedent(
    """\
    #!/usr/bin/env bash
    env | sed 's/=.*//' | sort
    echo "HEADER:${__OCX_ENV_STATE%%.*}"
    """
)


def _plugin_dir(tmp_path: Path, name: str, body: str) -> Path:
    directory = tmp_path / "plugins dir"
    directory.mkdir(exist_ok=True)
    script = directory / name
    script.write_text(body)
    script.chmod(script.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return directory


def _plugin_run(
    ocx: OcxRunner, directory: Path, *args: str, extra: dict[str, str] | None = None
):
    env = {
        **ocx.env,
        "PATH": f"{directory}{os.pathsep}{ocx.env['PATH']}",
        **(extra or {}),
    }
    return _run(ocx, directory, *args, env=env)


@pytest.mark.parametrize(
    "credential",
    [
        "OCX_IDENTITY_TOKEN",
        "OCX_KEY_PASSWORD",
        "OCX_SIGNING_KEY",
        "OCX_ANNOUNCE_GIT_TOKEN",
    ],
)
def test_plugin_never_receives_a_bearer_credential(
    ocx: OcxRunner, tmp_path: Path, credential: str
) -> None:
    directory = _plugin_dir(tmp_path, "ocx-names", NAMES_PLUGIN)
    result = _plugin_run(
        ocx,
        directory,
        "names",
        extra={credential: SENTINEL, "OCX_TOOLCHAIN_DIR": str(tmp_path)},
    )
    assert result.returncode == 0, result.stderr
    names = result.stdout.splitlines()
    assert "OCX_TOOLCHAIN_DIR" in names, "control: the listing sees a plain variable"
    assert credential not in names


def test_plugin_receives_the_announce_token_and_toolchain_dir(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    directory = _plugin_dir(tmp_path, "ocx-names", NAMES_PLUGIN)
    result = _plugin_run(
        ocx,
        directory,
        "names",
        extra={"OCX_ANNOUNCE_TOKEN": SENTINEL, "OCX_TOOLCHAIN_DIR": str(tmp_path)},
    )
    assert result.returncode == 0, result.stderr
    names = result.stdout.splitlines()
    assert "OCX_ANNOUNCE_TOKEN" in names and "OCX_TOOLCHAIN_DIR" in names, names


@pytest.mark.parametrize("label", ["garbage", "other_major", "truncated", "empty"])
def test_plugin_dispatch_survives_an_unusable_ambient_state(
    ocx: OcxRunner, tmp_path: Path, label: str
) -> None:
    """A plugin still runs, exit 0, when the ambient state cannot be read."""
    ambient = {
        "garbage": "!!not-a-state!!",
        "other_major": exec_state.craft_other_major(exec_state.MAJOR + 1),
        "truncated": exec_state.encode(exec_state.default_state())[:20],
        "empty": "",
    }[label]
    directory = _plugin_dir(tmp_path, "ocx-names", NAMES_PLUGIN)
    result = _plugin_run(ocx, directory, "names", extra={CARRIER: ambient})
    assert result.returncode == 0, f"{result.returncode}: {result.stderr}"
    assert "HEADER:" in result.stdout


def test_plugin_state_carries_the_dispatching_projects_values_verbatim(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A plugin started under an exec sees the carrier as the exec composed it, `--env` values verbatim."""
    probe = _plugin_dir(tmp_path, "ocx-carrier", f"#!/bin/sh\nprintenv {CARRIER}\n")
    project = _project(ocx, tmp_path / "p", 'PLAIN_CONST = "plain-fixture-value"')
    env = {**ocx.env, "PATH": f"{probe}{os.pathsep}{ocx.env['PATH']}"}
    result = _run(
        ocx,
        project,
        "exec",
        "--env",
        f"DB_PASSWORD={SENTINEL}",
        "--",
        str(ocx.binary),
        "carrier",
        env=env,
    )
    assert result.returncode == 0, result.stderr
    assert _applied_literal(result.stdout, "DB_PASSWORD") == SENTINEL


def test_plugin_exit_code_survives_a_signal_free_failure(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    directory = _plugin_dir(tmp_path, "ocx-fail", "#!/bin/sh\nexit 7\n")
    assert _plugin_run(ocx, directory, "fail").returncode == 7


# ---------------------------------------------------------------------------
# package test --script
# ---------------------------------------------------------------------------

_PROBE_TOOL = '#!/bin/sh\nprintenv "$1"\n'


def _probe_package(ocx: OcxRunner, tmp_path: Path):
    repo = f"t_{uuid4().hex[:8]}_hardening_probe"
    pkg = make_package(
        ocx,
        repo,
        "1.0.0",
        tmp_path,
        bins=["probe"],
        bin_scripts={"probe": _PROBE_TOOL},
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
                "visibility": "public",
            }
        ],
        cascade=False,
    )
    bundle = tmp_path / f"bundle-{repo}-1.0.0.tar.xz"
    return bundle, resolved_metadata_path(bundle), pkg


def _script_run(ocx, tmp_path, source: str, extra: dict[str, str] | None = None):
    bundle, meta, pkg = _probe_package(ocx, tmp_path)
    script = tmp_path / "hardening.star"
    script.write_text(source)
    env = {**ocx.env, **(extra or {})}
    return subprocess.run(
        [
            str(ocx.binary),
            "package",
            "test",
            "-p",
            current_platform(),
            "-m",
            str(meta),
            "-i",
            pkg.short,
            str(bundle),
            "--script",
            str(script),
        ],
        capture_output=True,
        text=True,
        env=env,
        stdin=subprocess.DEVNULL,
        timeout=120,
        check=False,
    )


@pytest.mark.parametrize("credential", ["OCX_IDENTITY_TOKEN", "OCX_SIGNING_KEY"])
def test_script_child_never_sees_an_ambient_bearer_credential(
    ocx: OcxRunner, tmp_path: Path, registry: str, credential: str
) -> None:
    source = (
        'c = ocx.run("probe", "HARDENING_CONTROL", env={"HARDENING_CONTROL": "control-seen"})\n'
        'expect.true(c.stdout.strip() == "control-seen", msg="probe control not seen")\n'
        f'r = ocx.run("probe", "{credential}")\n'
        'expect.true(r.stdout.strip() == "", msg="credential reached a script child")\n'
        f'expect.true(ocx.env("{credential}") == None, msg="credential readable from the script")\n'
    )
    result = _script_run(ocx, tmp_path, source, extra={credential: SENTINEL})
    assert result.returncode == 0, f"{result.stdout}\n{result.stderr}"
    assert SENTINEL not in result.stdout + result.stderr


def test_script_child_state_carries_no_ambient_secret(
    ocx: OcxRunner, tmp_path: Path, registry: str
) -> None:
    """A script child sees a carrier that holds no secret from the host's `--env`."""
    source = (
        'c = ocx.run("probe", "HARDENING_CONTROL", env={"HARDENING_CONTROL": "control-seen"})\n'
        'expect.true(c.stdout.strip() == "control-seen", msg="probe control not seen")\n'
        f'r = ocx.run("probe", "{CARRIER}")\n'
        f'expect.true("{SENTINEL}" not in r.stdout, msg="secret in the child state")\n'
    )
    result = _script_run(ocx, tmp_path, source, extra={"DEPLOY_TOKEN": SENTINEL})
    assert result.returncode == 0, f"{result.stdout}\n{result.stderr}"


@pytest.mark.parametrize("key", ["OCX_HOME", "OCX_OFFLINE", "OCX_PROJECT"])
def test_script_env_overlay_of_an_ocx_control_variable_is_refused_or_inert(
    ocx: OcxRunner, tmp_path: Path, registry: str, key: str
) -> None:
    """Overlaying a control variable never redirects the child's ocx home."""
    source = (
        # Positive control: the probe prints an overlaid ordinary variable, so the
        # refusal below is the control variable's doing, not a broken script.
        'c = ocx.run("probe", "HARDENING_CONTROL", env={"HARDENING_CONTROL": "control-seen"})\n'
        'expect.true(c.stdout.strip() == "control-seen", msg="probe control not seen")\n'
        f'ocx.run("probe", "{key}", env={{"{key}": "/forged/location"}})\n'
    )
    result = _script_run(ocx, tmp_path, source)
    assert result.returncode != 0, f"{result.stdout}\n{result.stderr}"
    assert f"cannot override the reserved key '{key}'" in result.stdout + result.stderr
    assert "probe control not seen" not in result.stdout + result.stderr


# ---------------------------------------------------------------------------
# Resilience of exporters to a hostile inherited state
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("label", ["garbage", "other_major", "empty"])
@pytest.mark.parametrize("command", [("env", "--shell=bash"), ("direnv", "export")])
def test_exporters_survive_an_unusable_ambient_state(
    ocx: OcxRunner, tmp_path: Path, label: str, command: tuple[str, ...]
) -> None:
    ambient = {
        "garbage": "!!not-a-state!!",
        "other_major": exec_state.craft_other_major(exec_state.MAJOR + 1),
        "empty": "",
    }[label]
    project = _project(ocx, tmp_path / "p", 'PLAIN_CONST = "plain-fixture-value"')
    done = _run(ocx, project, *command, env={**ocx.env, CARRIER: ambient})
    assert done.returncode == 0, done.stderr
    assert "PLAIN_CONST" in done.stdout
