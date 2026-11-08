"""Acceptance tests for what a spawned child receives from `ocx exec` (`adr_exec_env_model.md`).

A child holds no public `OCX_*` variable that ocx wrote. Two variables cross to a descendant:
`OCX_BINARY_PIN` (the running binary) and `__OCX_ENV_STATE` (the execution state), whose header and
policy carry the five root flags (`--offline`, `--frozen`, `--remote`, `--config`, `--index`), a
home, and any ambient or received value a `--clean` child would otherwise lose. A subcommand flag
such as `--no-verify` is never written. The receiver side is `test_env_received_policy.py`.
"""

from __future__ import annotations

import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

import pytest

from src import exec_state
from src.helpers import make_package_with_entrypoints
from src.runner import OcxRunner

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="OCX configuration forwarding is exercised via POSIX shell helpers (`sh`, `cat`); Windows has the same Rust surface covered by unit tests",
)
pytestmark = [pytestmark, pytest.mark.command("exec")]


def _exec_capture(
    ocx: OcxRunner,
    pkg_short: str,
    cmd: list[str],
    *outer_flags: str,
    extra_env: dict[str, str] | None = None,
    stdin: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run `ocx [outer_flags] exec pkg -- cmd...` capturing stdout/stderr."""
    full = [str(ocx.binary), *outer_flags, "package", "exec", pkg_short, "--", *cmd]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        full,
        capture_output=True,
        text=True,
        env=env,
        input=stdin,
        timeout=30,
        check=False,
    )


_PIN_AND_STATE = {"OCX_BINARY_PIN", "__OCX_ENV_STATE"}
_PRINT_STATE = 'printf "%s" "$__OCX_ENV_STATE"'


def _run_in_child(
    ocx: OcxRunner,
    pkg_short: str,
    command: list[str],
    *,
    outer_flags: tuple[str, ...] = (),
    mode_flags: tuple[str, ...] = (),
    env: dict[str, str | None] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run `ocx [outer_flags] package exec [mode_flags] pkg -- command...`.

    `env` overlays the runner's environment; a `None` value removes the key. Absolute
    interpreter paths in `command`: `--clean` drops the inherited `PATH`.
    """
    merged: dict[str, str] = dict(ocx.env)
    for key, value in (env or {}).items():
        if value is None:
            merged.pop(key, None)
        else:
            merged[key] = value
    return subprocess.run(
        [
            str(ocx.binary),
            *outer_flags,
            "package",
            "exec",
            *mode_flags,
            pkg_short,
            "--",
            *command,
        ],
        capture_output=True,
        text=True,
        env=merged,
        stdin=subprocess.DEVNULL,
        timeout=30,
        check=False,
    )


def _child_state(ocx: OcxRunner, pkg_short: str, **kwargs) -> dict:
    """The decoded `__OCX_ENV_STATE` the child of one `package exec` receives."""
    result = _run_in_child(ocx, pkg_short, ["/bin/sh", "-c", _PRINT_STATE], **kwargs)
    assert result.returncode == 0, result.stderr
    state = exec_state.decode(result.stdout)
    assert isinstance(state, dict), (
        f"the child must receive a readable execution state; got {result.stdout!r}"
    )
    return state


def _child_ocx_names(ocx: OcxRunner, pkg_short: str, **kwargs) -> set[str]:
    """The `OCX_*` / `__OCX_*` variable names present in the child's environment."""
    result = _run_in_child(ocx, pkg_short, ["/usr/bin/env", "-0"], **kwargs)
    assert result.returncode == 0, result.stderr
    names = {entry.split("=", 1)[0] for entry in result.stdout.split("\0") if entry}
    return {name for name in names if name.startswith(("OCX_", "__OCX_"))}


def _ambient_ocx_names(ocx: OcxRunner) -> set[str]:
    """What the runner itself exports: the parent's own values, not something ocx wrote."""
    return {name for name in ocx.env if name.startswith(("OCX_", "__OCX_"))}


def _received(**policy: object) -> str:
    """A state a parent ocx could have handed down, carrying `policy`."""
    return exec_state.encode({**exec_state.default_state(), "policy": policy})


def test_ocx_binary_set_on_child_env(ocx: OcxRunner, published_package) -> None:
    """`ocx exec` writes `OCX_BINARY_PIN=<absolute-path-to-running-ocx>` onto the child."""
    ocx.plain("package", "install", published_package.short)
    result = _exec_capture(
        ocx, published_package.short, ["sh", "-c", 'printf "%s" "$OCX_BINARY_PIN"']
    )
    assert result.returncode == 0, result.stderr
    assert Path(result.stdout).resolve() == ocx.binary.resolve(), (
        f"OCX_BINARY_PIN must equal the running ocx binary; got {result.stdout!r}, expected {ocx.binary}"
    )


_ROOT_FLAGS: list = [
    pytest.param(
        lambda tmp: ("--offline",),
        lambda state, tmp: "offline" in state["narrowing"],
        id="offline",
    ),
    pytest.param(
        lambda tmp: ("--frozen",),
        lambda state, tmp: "frozen" in state["narrowing"],
        id="frozen",
    ),
    pytest.param(
        lambda tmp: ("--remote",),
        lambda state, tmp: state["policy"].get("remote") is True,
        id="remote",
    ),
    pytest.param(
        lambda tmp: ("--config", str(tmp / "config.toml")),
        lambda state, tmp: (
            state["policy"].get("config_path") == str(tmp / "config.toml")
        ),
        id="config",
    ),
]


@pytest.mark.parametrize(
    "mode_flags", [(), ("--isolate",), ("--clean",)], ids=["layer", "isolate", "clean"]
)
@pytest.mark.parametrize("flag, written", _ROOT_FLAGS)
def test_root_flag_travels_in_the_state_never_in_a_public_variable(
    ocx: OcxRunner,
    published_package,
    tmp_path: Path,
    flag: Callable[[Path], tuple[str, ...]],
    written: Callable[[dict, Path], bool],
    mode_flags: tuple[str, ...],
) -> None:
    """A root flag reaches the child through the execution state's header or policy.

    No `OCX_OFFLINE` / `OCX_FROZEN` / `OCX_REMOTE` / `OCX_CONFIG` is written, whatever the
    mode: the child holds the runner's own ambient variables (none under `--clean`), the
    binary pin and the state, and nothing else. The runner exports none of the flag
    variables, so a name beyond that set was written by ocx.
    """
    (tmp_path / "config.toml").write_text("", encoding="utf-8")
    ocx.plain("package", "install", published_package.short)
    outer = flag(tmp_path)

    names = _child_ocx_names(
        ocx, published_package.short, outer_flags=outer, mode_flags=mode_flags
    )
    expected = _PIN_AND_STATE | (
        set() if mode_flags == ("--clean",) else _ambient_ocx_names(ocx)
    )
    assert names == expected, (
        f"{outer} {mode_flags}: the child must hold only {sorted(expected)}; "
        f"extra: {sorted(names - expected)}, missing: {sorted(expected - names)}"
    )
    state = _child_state(
        ocx, published_package.short, outer_flags=outer, mode_flags=mode_flags
    )
    assert written(state, tmp_path), (
        f"{outer} must be written into the child's state; header narrowing={state['narrowing']!r}, "
        f"policy={state['policy']!r}"
    )


def test_clean_child_holds_the_pin_and_the_state_but_no_home_variable(
    ocx: OcxRunner, published_package
) -> None:
    """Under `--clean` the child starts empty: `OCX_BINARY_PIN` names the running ocx and the
    state carries the home. No ambient parent-shell export can leak in or override this."""
    ocx.plain("package", "install", published_package.short)
    result = _run_in_child(
        ocx,
        published_package.short,
        ["/bin/sh", "-c", 'printf "%s" "${OCX_BINARY_PIN-unset}"'],
        outer_flags=("--offline",),
        mode_flags=("--clean",),
    )
    assert result.returncode == 0, result.stderr
    assert Path(result.stdout).resolve() == ocx.binary.resolve(), (
        f"`OCX_BINARY_PIN` must be the running ocx under --clean; got {result.stdout!r}"
    )
    state = _child_state(
        ocx,
        published_package.short,
        outer_flags=("--offline",),
        mode_flags=("--clean",),
    )
    assert Path(state["home"]).resolve() == Path(ocx.ocx_home).resolve(), (
        f"a --clean child must find the home through the state header; got {state['home']!r}"
    )


def test_clean_encapsulates_parent_env(ocx: OcxRunner, published_package) -> None:
    """Under `--clean`, parent-shell vars not in the execution state or the
    package env do not leak past the clean boundary. Regression guard for
    `Command::env_clear()` on the spawn/exec helper — without it,
    `Command::envs()` merges into the inherited parent env and `--clean`
    becomes a no-op for arbitrary `STRAY_VAR` exports."""
    ocx.plain("package", "install", published_package.short)
    # `--clean` drops PATH; hand `Command::new` an absolute interpreter path
    # for the same reason as `test_clean_env_writes_ocx_config_explicitly`.
    sh_path = "/bin/sh"
    full = [
        str(ocx.binary),
        "package",
        "exec",
        "--clean",
        published_package.short,
        "--",
        sh_path,
        "-c",
        'printf "%s" "${STRAY_VAR-unset}"',
    ]
    env = dict(ocx.env)
    env["STRAY_VAR"] = "leak"
    result = subprocess.run(
        full,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "unset", (
        f"`STRAY_VAR` from parent shell must not leak past `--clean`; got {result.stdout!r}"
    )


def test_exec_inherits_stdin_by_default(ocx: OcxRunner, published_package) -> None:
    """Stdin is inherited by `ocx exec`'s child unconditionally; the
    `--interactive` flag was removed because the new default matches shell
    exec semantics."""
    ocx.plain("package", "install", published_package.short)
    payload = "hello-from-stdin"
    result = _exec_capture(ocx, published_package.short, ["cat"], stdin=payload)
    assert result.returncode == 0, result.stderr
    assert result.stdout == payload, (
        f"stdin must flow through `ocx exec` to the child; got stdout={result.stdout!r}"
    )


def test_interactive_flag_rejected(ocx: OcxRunner, published_package) -> None:
    """The `--interactive` / `-i` flag was removed; clap rejects it as a
    usage error (sysexits.h `EX_USAGE = 64`)."""
    ocx.plain("package", "install", published_package.short)
    full = [
        str(ocx.binary),
        "package",
        "exec",
        "--interactive",
        published_package.short,
        "--",
        "true",
    ]
    result = subprocess.run(
        full,
        capture_output=True,
        text=True,
        env=dict(ocx.env),
        timeout=30,
        check=False,
    )
    assert result.returncode == 64, (
        f"removed `--interactive` flag must surface as UsageError (64); got rc={result.returncode}, stderr={result.stderr!r}"
    )


def test_generated_launcher_uses_ocx_binary_and_silences_presentation(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The Unix launcher template uses `${OCX_BINARY_PIN:-ocx}` and bakes the
    `launcher exec` subcommand so the entrypoint chain is opaque to the
    surrounding tool. Presentation overrides (--log-level=off / --color=never /
    --format=plain) and self-view are forced inside `launcher exec` and are
    NOT baked per-launcher; the launcher body must therefore contain neither
    presentation flags nor `--self`. Inspecting the generated file is the
    cheapest assertion of the One-Way Door wire vocabulary committed in
    `adr_package_entry_points.md` §Stable Surfaces."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    from src.runner import registry_dir

    reg = registry_dir(ocx.registry)
    launcher = (
        Path(str(ocx.ocx_home))
        / "symlinks"
        / reg
        / pkg.repo
        / "current"
        / "entrypoints"
        / "hello"
    )
    assert launcher.exists(), f"launcher must exist: {launcher}"
    body = launcher.read_text()
    assert '"${OCX_BINARY_PIN:-ocx}"' in body, (
        f"launcher must invoke inner ocx via `${{OCX_BINARY_PIN:-ocx}}`: {body!r}"
    )
    assert "launcher exec" in body, (
        f"launcher must call the `launcher exec` subcommand: {body!r}"
    )
    for forbidden in ("--log-level", "--color", "--format", "--self", "file://"):
        assert forbidden not in body, (
            f"launcher must NOT bake `{forbidden}` (hidden inside `launcher exec`): {body!r}"
        )


def test_exec_child_exit_code_propagates(ocx: OcxRunner, published_package) -> None:
    """Non-zero child exit codes propagate verbatim through `ocx exec` on Unix.

    On Unix, `ocx exec` uses `execvp`-style process replacement, so the child's
    exit code becomes the ocx process's exit code with no wrapping or saturation.
    The canonical case is exit code 42 — an application-specific code above the
    shell-reserved range (1–2) and below the sysexits range (64+).

    Windows note: Windows spawn uses `.status()` with `.unwrap_or(1)` fallback,
    so non-zero codes above 1 are not guaranteed to propagate identically. That
    path is covered by the `utility::child_process` unit tests; this module is
    Unix-only per the module-level `pytestmark`.
    """
    ocx.plain("package", "install", published_package.short)
    result = _exec_capture(ocx, published_package.short, ["sh", "-c", "exit 42"])
    assert result.returncode == 42, (
        f"`ocx exec` must propagate child exit code 42 verbatim (Unix execvp passthrough); "
        f"got rc={result.returncode}, stderr={result.stderr!r}"
    )


def test_launcher_chain_offline_propagates(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """End-to-end: `ocx --offline exec pkg -- hello` flows through the
    generated launcher's inner `ocx launcher exec` call, and the
    inner ocx reads the outer's `--offline` from the state header.

    The launcher's wrapped target is the package's `bin/hello` script; we
    do not need a separate observation point — what we are asserting is that
    `--offline` does not break the launcher chain (i.e. the outer's offline
    policy reaches the inner ocx without forcing it into a no-network
    failure mode for purely local resolution work).
    """
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=["hello"],
        bins=["hello"],
    )
    ocx.plain("package", "install", "--select", pkg.short)

    full = [
        str(ocx.binary),
        "--offline",
        "package",
        "exec",
        pkg.short,
        "--",
        "hello",
    ]
    env = dict(ocx.env)
    result = subprocess.run(
        full,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, (
        f"launcher chain must succeed under outer --offline (purely local resolution work); rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert pkg.marker in result.stdout, (
        f"launcher must invoke wrapped target; stdout={result.stdout!r}"
    )


def test_bare_env_name_passes_the_invoking_value_through_under_clean(
    ocx: OcxRunner, published_package
) -> None:
    """`--env NAME`, with no `=`, copies the invoking process's own `NAME`
    into the child — the Docker `-e NAME` convention.

    Asserted under `--clean`, where the child environment starts empty, so a
    pass-through is the only way one of the caller's variables reaches the far
    side. `/bin/sh` is named absolutely for the same reason the other `--clean`
    rows in this module do: `--clean` drops the inherited `PATH`.
    """
    ocx.plain("package", "install", published_package.short)
    result = ocx.plain(
        "package",
        "exec",
        "--clean",
        "--env",
        "PASSTHROUGH_FOO",
        published_package.short,
        "--",
        "/bin/sh",
        "-c",
        'printf "%s" "${PASSTHROUGH_FOO-unset}"',
        env_overrides={"PASSTHROUGH_FOO": "carried-from-the-parent"},
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "carried-from-the-parent", (
        "a bare `--env NAME` must carry the invoking process's value into a "
        f"--clean child; got {result.stdout!r}"
    )


def test_bare_env_name_unset_in_the_parent_is_not_an_error(
    ocx: OcxRunner, published_package
) -> None:
    """A name the invoking process does not set contributes nothing, and the
    invocation still succeeds.

    An allowlist names what *may* travel, so most of what it names is typically
    absent; refusing — or warning — would fire on the common case.
    """
    ocx.plain("package", "install", published_package.short)
    result = ocx.plain(
        "package",
        "exec",
        "--clean",
        "--env",
        "PASSTHROUGH_ABSENT",
        published_package.short,
        "--",
        "/bin/sh",
        "-c",
        'printf "%s" "${PASSTHROUGH_ABSENT-unset}"',
        check=False,
    )
    assert result.returncode == 0, (
        f"an unset pass-through name must not fail the invocation; stderr={result.stderr!r}"
    )
    assert result.stdout == "unset", (
        f"an unset name must reach the child unset, not as an empty string; got {result.stdout!r}"
    )


def test_bare_env_name_in_the_reserved_namespace_is_a_usage_error(
    ocx: OcxRunner, published_package
) -> None:
    """`OCX_*` stays refused in the bare form too (sysexits.h `EX_USAGE = 64`).

    Policy travels in the execution state; a user allowlist is never the way
    in, whichever spelling it uses.
    """
    ocx.plain("package", "install", published_package.short)
    result = ocx.plain(
        "package",
        "exec",
        "--env",
        "OCX_OFFLINE",
        published_package.short,
        "--",
        "true",
        check=False,
    )
    assert result.returncode == 64, (
        "a reserved bare `--env` name must be a usage error, not a silent skip; "
        f"got rc={result.returncode}, stderr={result.stderr!r}"
    )


def test_qualified_env_name_without_a_value_is_a_usage_error(
    ocx: OcxRunner, published_package
) -> None:
    """Only the *plain* bare form passes through: `--env FOO:path` declares a
    modifier with nothing to apply it to, and stays exit 64."""
    ocx.plain("package", "install", published_package.short)
    result = ocx.plain(
        "package",
        "exec",
        "--env",
        "PASSTHROUGH_FOO:path",
        published_package.short,
        "--",
        "true",
        check=False,
    )
    assert result.returncode == 64, (
        "a `:TYPE` with no value must stay a usage error; "
        f"got rc={result.returncode}, stderr={result.stderr!r}"
    )


def test_ambient_project_prune_is_written_into_a_clean_childs_state(
    ocx: OcxRunner, published_package
) -> None:
    """`OCX_NO_PROJECT` and `OCX_NO_CONFIG_REFRESH` are ambient-only knobs with no CLI flag.

    A `--clean` child drops the variables, so the state must carry what the parent
    resolved; otherwise the child re-adopts the project the parent pruned. Neither
    variable is written into the child.
    """
    ocx.plain("package", "install", published_package.short)
    overrides: dict[str, str | None] = {
        "OCX_NO_PROJECT": "1",
        "OCX_NO_CONFIG_REFRESH": "1",
    }

    state = _child_state(
        ocx, published_package.short, mode_flags=("--clean",), env=overrides
    )
    assert state["policy"].get("no_project") is True, state["policy"]
    assert state["policy"].get("no_config_refresh") is True, state["policy"]
    names = _child_ocx_names(
        ocx, published_package.short, mode_flags=("--clean",), env=overrides
    )
    assert names == _PIN_AND_STATE, (
        f"the knobs must travel in the state, not as variables; got {sorted(names)}"
    )


def test_ambient_default_registry_is_written_into_a_clean_childs_state(
    ocx: OcxRunner, published_package
) -> None:
    """`OCX_DEFAULT_REGISTRY` decides what repository a bare identifier names, so a
    child resolving it differently resolves a different package."""
    ocx.plain("package", "install", published_package.short)
    state = _child_state(ocx, published_package.short, mode_flags=("--clean",))
    assert state["policy"].get("default_registry") == ocx.registry, (
        "the default registry the parent resolved under must travel in the state; "
        f"expected {ocx.registry!r}, got {state['policy']!r}"
    )


@pytest.mark.parametrize("hermetic", [False, True], ids=["ambient", "no-config"])
def test_ambient_toolchain_dir_is_written_into_a_clean_childs_state(
    ocx: OcxRunner, published_package, hermetic: bool
) -> None:
    """A `toolchain_dir` root set only by `OCX_TOOLCHAIN_DIR` must reach a child ocx,
    hermetic or not; without it the child renders its project tree somewhere else than
    the parent did."""
    ocx.plain("package", "install", published_package.short)
    # The root is refused under a group- or world-writable ancestor; a 002 umask makes the home one.
    Path(ocx.ocx_home).chmod(0o755)
    root = Path(ocx.ocx_home).resolve() / "tc"
    overrides: dict[str, str | None] = {"OCX_TOOLCHAIN_DIR": str(root)}
    if hermetic:
        overrides["OCX_NO_CONFIG"] = "1"
    state = _child_state(
        ocx, published_package.short, mode_flags=("--clean",), env=overrides
    )
    assert state["policy"].get("toolchain_dir") == str(root), (
        "the toolchain root the parent resolved from the environment must travel in "
        f"the state; expected {str(root)!r}, got {state['policy']!r}"
    )


def test_ambient_insecure_registries_are_written_into_a_clean_childs_state(
    ocx: OcxRunner, published_package
) -> None:
    """`OCX_INSECURE_REGISTRIES` decides which authorities may be dialled over plain HTTP.
    A child with a narrower set refuses the very registry the parent just pulled from.

    The extra authority is a host nothing in this test contacts; it is there so the
    assertion pins the forwarded value rather than a string the child could have
    derived some other way.
    """
    ocx.plain("package", "install", published_package.short)
    authorities = f"{ocx.registry},never-dialled.example:5000"
    state = _child_state(
        ocx,
        published_package.short,
        mode_flags=("--clean",),
        env={"OCX_INSECURE_REGISTRIES": authorities},
    )
    assert state["policy"].get("insecure_registries") == authorities, (
        "the insecure-authority list must travel in the state verbatim; "
        f"expected {authorities!r}, got {state['policy']!r}"
    )


def test_ambient_kill_switches_absent_from_a_clean_childs_state_when_unset(
    ocx: OcxRunner, published_package
) -> None:
    """The other half of the write rule: a knob the parent did not resolve must not appear
    in the child's policy, so the write can never be read as unconditional."""
    ocx.plain("package", "install", published_package.short)
    state = _child_state(
        ocx,
        published_package.short,
        mode_flags=("--clean",),
        env={"OCX_NO_PROJECT": None, "OCX_NO_CONFIG_REFRESH": None},
    )
    assert "no_project" not in state["policy"], state["policy"]
    assert "no_config_refresh" not in state["policy"], state["policy"]


def test_layer_child_inherits_ambient_values_and_the_state_repeats_none_of_them(
    ocx: OcxRunner, published_package
) -> None:
    """Ambient values reach a layered child through the inherited environment itself; the
    state carries only root flags and received values, so the same value is not stored twice."""
    ocx.plain("package", "install", published_package.short)
    result = _run_in_child(
        ocx,
        published_package.short,
        ["/bin/sh", "-c", 'printf "%s" "${OCX_DEFAULT_REGISTRY-unset}"'],
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == ocx.registry, (
        f"a layered child keeps the parent's own variable; got {result.stdout!r}"
    )
    state = _child_state(ocx, published_package.short)
    assert "default_registry" not in state["policy"], state["policy"]


def test_no_verify_is_not_part_of_an_ordinary_childs_policy(
    ocx: OcxRunner, published_package
) -> None:
    """`--no-verify` is a subcommand flag, and the write rule admits five root flags only.

    No command that spawns accepts it, so the property is that a child never carries a
    `no_verify` nobody handed this process: the policy is absent in every mode.
    """
    ocx.plain("package", "install", published_package.short)
    for mode_flags in ((), ("--isolate",), ("--clean",)):
        state = _child_state(ocx, published_package.short, mode_flags=mode_flags)
        assert "no_verify" not in state["policy"], (mode_flags, state["policy"])


def test_received_no_verify_passes_through_a_layered_child(
    ocx: OcxRunner, published_package
) -> None:
    """A value a parent wrote is passed on: a descendant of a `no_verify` frame stays in it."""
    ocx.plain("package", "install", published_package.short)
    state = _child_state(
        ocx,
        published_package.short,
        env={"__OCX_ENV_STATE": _received(no_verify=True)},
    )
    assert state["policy"].get("no_verify") is True, state["policy"]


def test_clean_child_shows_the_local_no_verify_override(
    ocx: OcxRunner, published_package
) -> None:
    """With a received `no_verify` and an explicit local `OCX_NO_VERIFY=0`, a `--clean`
    child's policy says `no_verify: false`. The clean environment drops the variable, so the
    state must carry the value this process itself resolved; the local override wins."""
    ocx.plain("package", "install", published_package.short)
    state = _child_state(
        ocx,
        published_package.short,
        mode_flags=("--clean",),
        env={"__OCX_ENV_STATE": _received(no_verify=True), "OCX_NO_VERIFY": "0"},
    )
    assert state["policy"].get("no_verify") is False, state["policy"]


def test_clean_child_keeps_a_received_no_verify_nobody_overrode(
    ocx: OcxRunner, published_package
) -> None:
    """The unopposed half of the override: without a local value the received one stays."""
    ocx.plain("package", "install", published_package.short)
    state = _child_state(
        ocx,
        published_package.short,
        mode_flags=("--clean",),
        env={"__OCX_ENV_STATE": _received(no_verify=True), "OCX_NO_VERIFY": None},
    )
    assert state["policy"].get("no_verify") is True, state["policy"]


def test_received_offline_survives_a_descendants_local_zero(
    ocx: OcxRunner, published_package
) -> None:
    """A descendant of `--offline` that sets `OCX_OFFLINE=0` stays offline.

    Narrowing booleans combine by OR, so a local `0` cannot undo what the parent
    narrowed. The inner ocx runs offline and its own child's header still says so.
    """
    ocx.plain("package", "install", published_package.short)
    inner = (
        f'OCX_OFFLINE=0 "$OCX_BINARY_PIN" package exec {published_package.short} -- '
        f"/bin/sh -c '{_PRINT_STATE}'"
    )
    result = _run_in_child(
        ocx,
        published_package.short,
        ["/bin/sh", "-c", inner],
        outer_flags=("--offline",),
    )
    assert result.returncode == 0, result.stderr
    state = exec_state.decode(result.stdout)
    assert isinstance(state, dict), result.stdout
    assert "offline" in state["narrowing"], (
        f"the descendant must stay offline; header narrowing={state['narrowing']!r}"
    )
