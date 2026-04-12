# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the OCX configuration system.

Covers: plan_configuration_system.md Step 3.4 — all user-experience scenarios
in the plan's "User Experience Scenarios" table and "Edge Cases" section.

Each test writes a config file into $OCX_HOME (the per-test isolated temp dir)
or passes it via --config / OCX_CONFIG_FILE, then runs an ocx command and
asserts the observable outcome.

Strategy for detecting which registry was resolved:
- Use `ocx install <bare-name>:0` (no registry prefix). When the command
  fails it prints the identifier it tried to resolve, which includes the
  default registry. We assert the correct registry hostname appears in stderr.
- Alternatively, for tests that just need to confirm *some* command runs
  successfully, use `ocx index catalog` which works even with an empty index.
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def write_home_config(ocx: OcxRunner, content: str) -> Path:
    """Write content to $OCX_HOME/config.toml."""
    path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    path.write_text(content)
    return path


def run_with_extra_env(
    ocx: OcxRunner,
    *args: str,
    extra_env: dict[str, str] | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[str]:
    """Run an ocx command with extra env vars merged into the runner env."""
    env = {**ocx.env}
    if extra_env:
        env.update(extra_env)
    cmd = [str(ocx.binary), "--format", "json"] + list(args)
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


def run_plain_with_extra_env(
    ocx: OcxRunner,
    *args: str,
    extra_env: dict[str, str] | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[str]:
    """Run an ocx command (no --format flag) with extra env vars."""
    env = {**ocx.env}
    if extra_env:
        env.update(extra_env)
    cmd = [str(ocx.binary)] + list(args)
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


# ---------------------------------------------------------------------------
# Tests: config file changes default registry (Step 3.4)
# ---------------------------------------------------------------------------


def test_config_default_registry_takes_effect(ocx: OcxRunner) -> None:
    """$OCX_HOME/config.toml [registry] default changes which registry is used.

    Plan: UX scenario — `[registry] default = "x"` in $OCX_HOME/config.toml.
    The OcxRunner fixture always sets OCX_DEFAULT_REGISTRY to the test
    registry. We replace that env var with a bare env (removing it) and write
    a config file, then verify the config value is reflected in output.

    Since we cannot easily push to altreg.example, we run `ocx install
    nonexistent:0` and assert the error message refers to the configured
    registry hostname.
    """
    write_home_config(ocx, '[registry]\ndefault = "altreg.example"\n')

    # Remove OCX_DEFAULT_REGISTRY so config file is the only source
    env = {k: v for k, v in ocx.env.items() if k != "OCX_DEFAULT_REGISTRY"}
    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of nonexistent package should fail"
    combined = result.stdout + result.stderr
    assert "altreg.example" in combined, (
        f"expected 'altreg.example' in output when config sets default registry, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_env_var_overrides_config_file(ocx: OcxRunner) -> None:
    """OCX_DEFAULT_REGISTRY env var takes precedence over config file.

    Plan: UX scenario — OCX_DEFAULT_REGISTRY=other.example with config setting
    altreg.example → env var wins.
    """
    write_home_config(ocx, '[registry]\ndefault = "altreg.example"\n')

    # Set OCX_DEFAULT_REGISTRY to override config
    env = {**ocx.env, "OCX_DEFAULT_REGISTRY": "envvar-wins.example"}
    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of nonexistent package should fail"
    combined = result.stdout + result.stderr
    assert "envvar-wins.example" in combined, (
        f"expected 'envvar-wins.example' in output (env var wins), "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "altreg.example" not in combined, (
        "config file registry should NOT appear when env var overrides"
    )


def test_no_config_kills_file_loading(ocx: OcxRunner) -> None:
    """OCX_NO_CONFIG=1 ignores all config files, reverts to env vars/defaults.

    Plan: UX scenario — OCX_NO_CONFIG=1 ignores $OCX_HOME/config.toml.
    """
    write_home_config(ocx, '[registry]\ndefault = "should-be-ignored.example"\n')

    # Use the test registry as OCX_DEFAULT_REGISTRY; config file should be ignored
    env = {**ocx.env, "OCX_NO_CONFIG": "1"}
    # OCX_DEFAULT_REGISTRY in env already points to the test registry (localhost:5000)
    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of nonexistent package should fail"
    combined = result.stdout + result.stderr
    assert "should-be-ignored.example" not in combined, (
        f"config file registry should be ignored when OCX_NO_CONFIG=1, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Tests: invalid config produces clear error (Step 3.4)
# ---------------------------------------------------------------------------


def test_invalid_config_produces_clear_error_with_path(ocx: OcxRunner) -> None:
    """Malformed TOML config → non-zero exit and error contains file path.

    Plan: UX scenario — invalid config file produces clear error with file path.
    Plan: Error taxonomy — Config::ParseError { path, source }.
    """
    config_path = write_home_config(ocx, "this is not valid toml =[[[")

    result = subprocess.run(
        [str(ocx.binary), "index", "catalog"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode != 0, (
        "command should fail when config file is invalid TOML"
    )
    combined = result.stdout + result.stderr
    assert str(config_path) in combined or "config.toml" in combined, (
        f"error message should contain the config file path, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Tests: --config flag and OCX_CONFIG_FILE env var (Step 3.4)
# ---------------------------------------------------------------------------


def test_explicit_config_flag_loads_file(ocx: OcxRunner, tmp_path: Path) -> None:
    """--config FILE loads the specified config file.

    Plan: UX scenario — `ocx --config /path/to/custom.toml install cmake:3.28`.
    """
    custom_config = tmp_path / "custom.toml"
    custom_config.write_text('[registry]\ndefault = "custom-flag.example"\n')

    env = {k: v for k, v in ocx.env.items() if k != "OCX_DEFAULT_REGISTRY"}
    result = subprocess.run(
        [str(ocx.binary), "--config", str(custom_config), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of nonexistent package should fail"
    combined = result.stdout + result.stderr
    assert "custom-flag.example" in combined, (
        f"expected 'custom-flag.example' from --config file, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_ocx_config_file_env_loads_file(ocx: OcxRunner, tmp_path: Path) -> None:
    """OCX_CONFIG_FILE env var loads only that config file.

    Plan: UX scenario — `OCX_CONFIG_FILE=/path/to/ci.toml ocx install cmake:3.28`.
    """
    ci_config = tmp_path / "ci.toml"
    ci_config.write_text('[registry]\ndefault = "ci-env.example"\n')

    env = {k: v for k, v in ocx.env.items() if k != "OCX_DEFAULT_REGISTRY"}
    env["OCX_CONFIG_FILE"] = str(ci_config)
    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of nonexistent package should fail"
    combined = result.stdout + result.stderr
    assert "ci-env.example" in combined, (
        f"expected 'ci-env.example' from OCX_CONFIG_FILE, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Tests: hermetic + explicit path combinations (Step 3.4)
# ---------------------------------------------------------------------------


def test_no_config_with_explicit_flag_loads_only_explicit(ocx: OcxRunner, tmp_path: Path) -> None:
    """OCX_NO_CONFIG=1 with --config → loads only the explicit file.

    The discovered chain (system/user/$OCX_HOME) is suppressed, but the
    explicit path is still honored. This is the hermetic-CI mode: "ignore
    ambient config, load only this one".
    """
    # Ambient $OCX_HOME config that MUST be ignored
    write_home_config(ocx, '[registry]\ndefault = "ambient.example"\n')

    hermetic = tmp_path / "hermetic.toml"
    hermetic.write_text('[registry]\ndefault = "hermetic.example"\n')

    # Remove OCX_DEFAULT_REGISTRY so the config value is observable.
    env = {k: v for k, v in ocx.env.items() if k != "OCX_DEFAULT_REGISTRY"}
    env["OCX_NO_CONFIG"] = "1"
    result = subprocess.run(
        [str(ocx.binary), "--config", str(hermetic), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of bogus package should fail"
    combined = result.stdout + result.stderr
    assert "hermetic.example" in combined, (
        f"expected 'hermetic.example' from explicit config, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "ambient.example" not in combined, (
        f"ambient $OCX_HOME config must be suppressed by OCX_NO_CONFIG=1, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_named_registry_table_resolves_default(ocx: OcxRunner) -> None:
    """[registry] default = "name" + [registries.name] url = "host" resolves to host.

    Plan: registries table — named entries provide a lookup target for
    `[registry] default`. When both are set, the default name is resolved
    through the registries map and the entry's `url` becomes the effective
    default registry.
    """
    write_home_config(
        ocx,
        '[registry]\ndefault = "company"\n\n'
        '[registries.company]\nurl = "registry-host.company.example"\n',
    )
    # Remove OCX_DEFAULT_REGISTRY so the resolved value is observable.
    env = {k: v for k, v in ocx.env.items() if k != "OCX_DEFAULT_REGISTRY"}
    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of nonexistent package should fail"
    combined = result.stdout + result.stderr
    assert "registry-host.company.example" in combined, (
        f"expected 'registry-host.company.example' (resolved via [registries.company]), "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_named_registry_with_no_url_falls_back_to_literal_name(ocx: OcxRunner) -> None:
    """[registry] default = "name" + [registries.name] without url → literal "name".

    Plan: resolved_default_registry fallback path — when the named entry
    exists but has no `url` field, the resolver falls back to treating the
    `default` value as a literal hostname. Backwards-compatible with bare
    hostnames when a future per-registry setting (e.g. `insecure`) is
    declared without a `url`.
    """
    write_home_config(
        ocx,
        '[registry]\ndefault = "literal-fallback.example"\n\n'
        '[registries."literal-fallback.example"]\n',
    )
    # Remove OCX_DEFAULT_REGISTRY so the resolved value is observable.
    env = {k: v for k, v in ocx.env.items() if k != "OCX_DEFAULT_REGISTRY"}
    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of nonexistent package should fail"
    combined = result.stdout + result.stderr
    assert "literal-fallback.example" in combined, (
        f"expected the literal name when [registries.<name>] has no url field, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_empty_ocx_config_file_is_escape_hatch(ocx: OcxRunner) -> None:
    """OCX_CONFIG_FILE="" is treated as unset, not as an error.

    This is the escape hatch for users with an ambient OCX_CONFIG_FILE in
    their shell environment that they want to disable for a single
    invocation without unsetting the variable.
    """
    write_home_config(ocx, '[registry]\ndefault = "home.example"\n')

    # Remove OCX_DEFAULT_REGISTRY so the config value is observable.
    env = {k: v for k, v in ocx.env.items() if k != "OCX_DEFAULT_REGISTRY"}
    env["OCX_CONFIG_FILE"] = ""
    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of bogus package should fail"
    combined = result.stdout + result.stderr
    assert "home.example" in combined, (
        f"expected 'home.example' from $OCX_HOME config when OCX_CONFIG_FILE is empty, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Tests: nonexistent explicit config → FileNotFound (Step 3.4)
# ---------------------------------------------------------------------------


def test_explicit_config_nonexistent_file_errors(ocx: OcxRunner) -> None:
    """--config pointing to nonexistent file → non-zero exit with path in error.

    Plan: UX scenario — `ocx --config /path/to/missing.toml install cmake:3.28`
    → `error: config file not found: /path/to/missing.toml`.
    """
    nonexistent = "/tmp/ocx-test-missing-config-99999.toml"
    result = subprocess.run(
        [str(ocx.binary), "--config", nonexistent, "index", "catalog"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode != 0, "missing --config file should fail"
    combined = result.stdout + result.stderr
    assert "ocx-test-missing-config-99999.toml" in combined, (
        f"error message should contain the missing file path, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_ocx_config_file_nonexistent_errors(ocx: OcxRunner) -> None:
    """OCX_CONFIG_FILE pointing to nonexistent file → non-zero exit with path in error.

    Plan: UX scenario row 6 — `OCX_CONFIG_FILE=/path/to/ci.toml` → file not
    found → "same error as --config".
    """
    nonexistent = "/tmp/ocx-test-missing-env-config-99999.toml"
    result = run_with_extra_env(
        ocx,
        "index",
        "catalog",
        extra_env={"OCX_CONFIG_FILE": nonexistent},
    )
    assert result.returncode != 0, "missing OCX_CONFIG_FILE should fail"
    combined = result.stdout + result.stderr
    assert "ocx-test-missing-env-config-99999.toml" in combined, (
        f"error message should contain the missing file path, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Tests: unknown top-level section ignored (Step 3.4 / edge case 1)
# ---------------------------------------------------------------------------


def test_unknown_top_level_section_ignored(ocx: OcxRunner) -> None:
    """Config with unknown top-level [future] section → no error, command runs.

    Plan: UX scenario — unknown top-level key [foo] silently ignored.
    Plan: Edge case — forward compatibility.
    """
    write_home_config(ocx, '[future]\nx = "y"\n')

    result = subprocess.run(
        [str(ocx.binary), "index", "catalog"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, (
        f"unknown top-level config section should not cause failure, "
        f"stderr={result.stderr!r}"
    )
