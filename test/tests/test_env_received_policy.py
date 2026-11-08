# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""What a child ocx keeps from the execution state it inherits.

Every case crafts the ``__OCX_ENV_STATE`` value with ``src.exec_state`` rather than running a
parent ocx, so each rule is pinned from the receiving side. Narrowing (``offline``) travels in the
plaintext header and a public ``OCX_OFFLINE=0`` cannot lift it; a relaxing received value yields to
an explicit ambient variable, with one warning; a header of another major, or an unreadable payload,
is still honoured. Offline is exit 81 (policy blocked) from ``package install`` of a never-indexed
tag; any other code means the registry was consulted.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from src import OcxRunner, exec_state
from src.exec_state import (
    craft_garbled_payload,
    craft_other_major,
    default_state,
    encode,
)

EXIT_POLICY_BLOCKED = 81
UNINDEXED = "nonexistent_received_policy_pkg:0"


def _run(
    ocx: OcxRunner,
    *args: str,
    env: dict[str, str] | None = None,
    drop: tuple[str, ...] = (),
) -> subprocess.CompletedProcess[str]:
    child_env = {k: v for k, v in ocx.env.items() if k not in drop}
    child_env.update(env or {})
    return subprocess.run(
        [str(ocx.binary), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=child_env,
        stdin=subprocess.DEVNULL,
        check=False,
        timeout=120,
    )


def _lines_naming(stderr: str, needle: str) -> list[str]:
    return [line for line in stderr.splitlines() if needle in line]


def _state_warnings(stderr: str) -> list[str]:
    """Stderr lines that talk about the inherited environment state."""
    return [
        line
        for line in stderr.splitlines()
        if "state" in line.lower() and line.startswith("ocx:")
    ]


def test_received_offline_survives_ambient_offline_zero(ocx: OcxRunner) -> None:
    """A received ``offline`` narrowing outlives a child's ``OCX_OFFLINE=0``."""
    state = encode(default_state(), narrowing=("offline",))
    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": state, "OCX_OFFLINE": "0"},
    )
    assert result.returncode == EXIT_POLICY_BLOCKED, (
        f"a received offline narrowing must keep the child offline despite OCX_OFFLINE=0; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )


def test_ambient_index_wins_over_received_index_with_one_warning(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``OCX_INDEX=/b`` over a received ``/a`` uses ``/b`` and warns once, naming the variable."""
    received = tmp_path / "a-index"
    received.write_text("a regular file: any index read under it fails\n")
    ambient = tmp_path / "b-index"
    ambient.mkdir()
    state = encode({**default_state(), "policy": {"index_dir": str(received)}})

    result = _run(
        ocx,
        "--offline",
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": state, "OCX_INDEX": str(ambient)},
    )

    assert result.returncode == EXIT_POLICY_BLOCKED, (
        f"the ambient index directory must be the one in use; rc={result.returncode}, stderr={result.stderr!r}"
    )
    naming = _lines_naming(result.stderr, "OCX_INDEX")
    assert len(naming) == 1, (
        f"exactly one warning must name OCX_INDEX; stderr={result.stderr!r}"
    )


def test_received_index_without_ambient_override_is_silent(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Control for the warning above: with no ambient ``OCX_INDEX`` nothing disagrees, so nothing is said."""
    received = tmp_path / "a-index"
    received.mkdir()
    state = encode({**default_state(), "policy": {"index_dir": str(received)}})

    result = _run(
        ocx,
        "--offline",
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": state},
    )

    assert result.returncode == EXIT_POLICY_BLOCKED, f"stderr={result.stderr!r}"
    assert not _lines_naming(result.stderr, "OCX_INDEX"), f"stderr={result.stderr!r}"


def test_other_major_header_keeps_offline_and_home_and_warns_once(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A major-3 state with ``n=offline`` and ``h=`` runs offline, finds the home, warns once."""
    home = tmp_path / "header-home"
    state = craft_other_major(3, narrowing=("offline",), home=str(home))

    blocked = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": state, "OCX_OFFLINE": "0"},
        drop=("OCX_HOME",),
    )
    assert blocked.returncode == EXIT_POLICY_BLOCKED, (
        f"the header's offline must hold under another major; rc={blocked.returncode}, stderr={blocked.stderr!r}"
    )
    assert len(_state_warnings(blocked.stderr)) == 1, (
        f"exactly one warning expected; stderr={blocked.stderr!r}"
    )

    reported = _run(
        ocx,
        "--offline",
        "--format",
        "json",
        "shell",
        "state",
        env={"__OCX_ENV_STATE": state},
        drop=("OCX_HOME",),
    )
    assert reported.returncode == 0, f"stderr={reported.stderr!r}"
    assert Path(json.loads(reported.stdout)["ocx_home"]) == home, (
        f"the header's home must be used when OCX_HOME is unset; stdout={reported.stdout!r}"
    )


def test_unreadable_payload_still_honours_the_header(ocx: OcxRunner) -> None:
    """A payload that fails to inflate loses its policy, never the header's ``offline``."""
    state = craft_garbled_payload("inflate", narrowing=("offline",))
    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": state, "OCX_OFFLINE": "0"},
    )
    assert result.returncode == EXIT_POLICY_BLOCKED, (
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert len(_state_warnings(result.stderr)) == 1, (
        f"exactly one warning expected; stderr={result.stderr!r}"
    )


def test_same_major_state_with_a_non_absolute_path_names_the_field_not_a_foreign_ocx(
    ocx: OcxRunner,
) -> None:
    """A state of this ocx's own major that fails validation is not "incompatible": the notice names the field."""
    payload = default_state()
    payload["session"]["project"] = {"dir": "relative/project", "key": "0" * 16}
    state = encode(payload, narrowing=("offline",))

    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": state, "OCX_OFFLINE": "0"},
    )

    assert result.returncode == EXIT_POLICY_BLOCKED, (
        f"the header's offline still holds; rc={result.returncode}, stderr={result.stderr!r}"
    )
    warnings = _state_warnings(result.stderr)
    assert len(warnings) == 1, f"exactly one warning expected; stderr={result.stderr!r}"
    assert "session.project.dir" in warnings[0], warnings
    assert "incompatible" not in warnings[0], warnings
    assert "unset __OCX_ENV_STATE" in warnings[0], warnings

    reported = _run(
        ocx,
        "--offline",
        "--format",
        "json",
        "shell",
        "state",
        env={"__OCX_ENV_STATE": state},
    )
    assert reported.returncode == 0, f"stderr={reported.stderr!r}"
    notes = json.loads(reported.stdout).get("notes", [])
    unreadable = [n for n in notes if n.get("type") == "env_state_unreadable"]
    assert unreadable and unreadable[0].get("field") == "session.project.dir", notes


EXIT_USAGE = 64


def test_received_config_path_reaches_the_child_without_the_public_variable(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A parent's ``--config`` travels in the state: the child loads that file though ``OCX_CONFIG`` is unset."""
    parent_config = tmp_path / "parent.toml"
    parent_config.write_text('[registry]\ndefault = "parent-config.example"\n')
    state = encode({**default_state(), "policy": {"config_path": str(parent_config)}})

    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": state},
        drop=("OCX_DEFAULT_REGISTRY", "OCX_CONFIG"),
    )

    assert "parent-config.example" in result.stdout + result.stderr, (
        f"the parent's --config must be loaded; stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_ambient_ocx_config_wins_over_the_received_config_path(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Relaxing precedence: an explicitly set ``OCX_CONFIG`` is the file in use; the parent's path is not loaded."""
    parent_config = tmp_path / "parent.toml"
    parent_config.write_text('[registry]\ndefault = "parent-config.example"\n')
    local_config = tmp_path / "local.toml"
    local_config.write_text('[registry]\ndefault = "local-config.example"\n')
    state = encode({**default_state(), "policy": {"config_path": str(parent_config)}})

    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": state, "OCX_CONFIG": str(local_config)},
        drop=("OCX_DEFAULT_REGISTRY",),
    )

    combined = result.stdout + result.stderr
    assert "local-config.example" in combined, (
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "parent-config.example" not in combined, (
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_received_remote_reaches_the_child_and_conflicts_with_ambient_frozen(
    ocx: OcxRunner,
) -> None:
    """A parent's ``--remote`` travels in the state, so a child's ``OCX_FROZEN=1`` contradicts it (exit 64)."""
    remote_state = encode({**default_state(), "policy": {"remote": True}})

    control = _run(ocx, "package", "install", UNINDEXED, env={"OCX_FROZEN": "1"})
    assert control.returncode != EXIT_USAGE, (
        f"control: frozen alone is no usage error; stderr={control.stderr!r}"
    )

    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": remote_state, "OCX_FROZEN": "1"},
    )
    assert result.returncode == EXIT_USAGE, (
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert "--frozen" in result.stderr, f"stderr={result.stderr!r}"


def _policy_state(**policy: object) -> str:
    return encode({**default_state(), "policy": policy})


def test_received_no_config_prunes_the_user_tier_in_the_child(ocx: OcxRunner) -> None:
    """A parent's ``OCX_NO_CONFIG`` travels in the state: the child skips ``$OCX_HOME/config.toml`` too."""
    (ocx.ocx_home / "config.toml").write_text(
        '[registry]\ndefault = "home-tier.example"\n'
    )
    drop = ("OCX_DEFAULT_REGISTRY", "OCX_CONFIG")

    control = _run(ocx, "package", "install", UNINDEXED, drop=drop)
    assert "home-tier.example" in control.stdout + control.stderr, (
        f"control: the home tier must be read; stdout={control.stdout!r} stderr={control.stderr!r}"
    )

    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": _policy_state(no_config=True)},
        drop=drop,
    )
    assert "home-tier.example" not in result.stdout + result.stderr, (
        f"a received no_config must keep the child hermetic; stdout={result.stdout!r} stderr={result.stderr!r}"
    )

    lifted = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": _policy_state(no_config=True), "OCX_NO_CONFIG": "0"},
        drop=drop,
    )
    assert "home-tier.example" in lifted.stdout + lifted.stderr, (
        f"an explicit OCX_NO_CONFIG=0 outranks the relaxing received value; stderr={lifted.stderr!r}"
    )


def test_received_managed_config_folds_the_snapshot_in_the_child(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A parent's ``OCX_MANAGED_CONFIG`` travels in the state: the loader folds the adopted snapshot."""
    from src.helpers import push_managed_config

    managed_repo = f"{unique_repo}_managed"
    push_managed_config(
        ocx,
        managed_repo,
        "v1",
        '[registry]\ndefault = "managed-tier.example"\n',
        tmp_path,
    )
    ref = f"{ocx.registry}/{managed_repo}:v1"
    ocx.run("config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref})
    drop = ("OCX_DEFAULT_REGISTRY", "OCX_CONFIG", "OCX_MANAGED_CONFIG")

    control = _run(ocx, "package", "install", UNINDEXED, drop=drop)
    assert "managed-tier.example" not in control.stdout + control.stderr, (
        f"control: without the source the snapshot is not folded; stderr={control.stderr!r}"
    )

    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={"__OCX_ENV_STATE": _policy_state(managed_config=ref)},
        drop=drop,
    )
    assert "managed-tier.example" in result.stdout + result.stderr, (
        f"a received managed_config must select the snapshot; stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_received_toolchain_dir_faces_the_environment_tier_refusals(
    ocx: OcxRunner,
) -> None:
    """A parent's ``OCX_TOOLCHAIN_DIR`` travels in the state and is validated like the variable (exit 78)."""
    control = _run(ocx, "package", "install", UNINDEXED, drop=("OCX_TOOLCHAIN_DIR",))
    assert control.returncode != 78, (
        f"control: no declared root is no refusal; stderr={control.stderr!r}"
    )

    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={
            "__OCX_ENV_STATE": _policy_state(
                toolchain_dir=exec_state.absolute("/usr/received-toolchains")
            )
        },
        drop=("OCX_TOOLCHAIN_DIR",),
    )
    assert result.returncode == 78, f"rc={result.returncode}, stderr={result.stderr!r}"
    assert "OCX_TOOLCHAIN_DIR" in result.stderr, (
        f"the refusal must name the tier; stderr={result.stderr!r}"
    )


def test_received_patches_select_the_patch_registry_in_the_child(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A parent's ``OCX_PATCHES`` travels in the state: ``patch publish`` finds a registry without ``--registry``."""
    needle = "no patch registry configured"
    args = (
        "patch",
        "publish",
        "--global",
        "--descriptor",
        str(tmp_path / "missing.json"),
    )

    control = _run(ocx, *args, drop=("OCX_PATCHES",))
    assert needle in control.stderr, (
        f"control: nothing configures a registry; stderr={control.stderr!r}"
    )

    state = _policy_state(patches=json.dumps({"registry": "patches.example"}))
    result = _run(ocx, *args, env={"__OCX_ENV_STATE": state}, drop=("OCX_PATCHES",))
    assert needle not in result.stderr, (
        f"a received patches value must configure the tier; stderr={result.stderr!r}"
    )


def test_received_no_update_check_reaches_the_background_gate(ocx: OcxRunner) -> None:
    """A parent's ``OCX_NO_UPDATE_CHECK`` travels in the state; an explicit ``0`` re-enables the check."""
    needle = "Update check skipped: OCX_NO_UPDATE_CHECK is set"
    log_env = {"OCX_LOG_LEVEL": "debug"}
    drop = ("OCX_NO_UPDATE_CHECK",)

    control = _run(ocx, "package", "install", UNINDEXED, env=log_env, drop=drop)
    assert needle not in control.stderr, f"control: stderr={control.stderr!r}"

    result = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={**log_env, "__OCX_ENV_STATE": _policy_state(no_update_check=True)},
        drop=drop,
    )
    assert needle in result.stderr, f"stderr={result.stderr!r}"

    lifted = _run(
        ocx,
        "package",
        "install",
        UNINDEXED,
        env={
            **log_env,
            "__OCX_ENV_STATE": _policy_state(no_update_check=True),
            "OCX_NO_UPDATE_CHECK": "0",
        },
    )
    assert needle not in lifted.stderr, f"stderr={lifted.stderr!r}"


def test_received_no_project_ends_the_project_walk_in_the_child(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A parent's ``OCX_NO_PROJECT`` travels in the state: the child does not adopt the project above its CWD."""
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text("[tools]\n")

    def run_in_project(state: str | None) -> subprocess.CompletedProcess[str]:
        env = {
            k: v
            for k, v in ocx.env.items()
            if k not in ("OCX_NO_PROJECT", "OCX_PROJECT", "__OCX_ENV_STATE")
        }
        if state is not None:
            env["__OCX_ENV_STATE"] = state
        return subprocess.run(
            [str(ocx.binary), "env"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=env,
            cwd=project,
            stdin=subprocess.DEVNULL,
            check=False,
            timeout=120,
        )

    control = run_in_project(None)
    assert "ocx.lock not found" in control.stderr, (
        f"control: the walk adopts the project; stderr={control.stderr!r}"
    )
    result = run_in_project(_policy_state(no_project=True))
    assert "no ocx.toml found" in result.stderr, (
        f"a received no_project must end the walk at the CWD; stderr={result.stderr!r}"
    )
