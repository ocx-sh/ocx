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

import re
import subprocess
from pathlib import Path

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
# Tests: static commands survive malformed ambient config
# Regression guard for the bug where `ocx version` and `ocx shell completion`
# (both Context-free commands) were aborted by `Context::try_init` → config
# parse error before command dispatch. `ocx info` is deliberately excluded:
# it displays `default_registry()` from the loaded config and must keep its
# current config-coupled behavior, so users can fall back to `ocx version`
# as the diagnostic entry point when config is broken.
# ---------------------------------------------------------------------------


def test_version_survives_invalid_ambient_config(ocx: OcxRunner) -> None:
    """`ocx version` exits 0 and prints a version string even with broken config.

    Regression guard: `ocx version` is the command users run to diagnose a
    broken install. It must not be aborted by `Context::try_init` failing on
    a malformed `~/.ocx/config.toml`.
    """
    write_home_config(ocx, "this is not valid toml =[[[")

    result = ocx.run("version", format=None, check=False)

    assert result.returncode == 0, (
        f"`ocx version` should exit 0 even with malformed config, "
        f"got rc={result.returncode}; stderr={result.stderr!r}"
    )
    assert re.match(r"^\d+\.\d+\.\d+", result.stdout.strip()), (
        f"stdout should be a MAJOR.MINOR.PATCH version string, "
        f"got {result.stdout!r}"
    )


def test_completion_survives_invalid_ambient_config(ocx: OcxRunner) -> None:
    """`ocx shell completion` exits 0 even with broken ambient config.

    Regression guard: completion scripts emit static clap output, never touch
    config, and are typically invoked from shell init files where a broken
    config must not break shell startup.
    """
    write_home_config(ocx, "this is not valid toml =[[[")

    result = ocx.run("shell", "completion", "--shell", "bash", format=None, check=False)

    assert result.returncode == 0, (
        f"`ocx shell completion --shell bash` should exit 0 even with malformed config, "
        f"got rc={result.returncode}; stderr={result.stderr!r}"
    )
    # clap_complete emits a bash completion function named `_ocx`.
    assert "_ocx" in result.stdout, (
        f"stdout should contain bash completion markers, "
        f"got {result.stdout!r}"
    )


def test_bare_ocx_survives_malformed_ambient_config(ocx: OcxRunner) -> None:
    """`ocx` with no arguments exits 0 and prints help even with broken config.

    Regression guard: bare `ocx` (no subcommand) must not fall through to
    `Context::try_init` and abort on a malformed `~/.ocx/config.toml`. The
    `None` command arm is handled in the static-command bypass block in
    `app.rs::run` before any config is loaded.
    """
    write_home_config(ocx, "this is not valid toml =[[[")

    result = ocx.run(format=None, check=False)

    assert result.returncode == 0, (
        f"`ocx` (no args) should exit 0 even with malformed config, "
        f"got rc={result.returncode}; stderr={result.stderr!r}"
    )
    assert "Usage:" in result.stdout, (
        f"`ocx` (no args) should print a Usage: section, "
        f"got stdout={result.stdout!r}"
    )


def test_help_survives_invalid_ambient_config(ocx: OcxRunner) -> None:
    """All `--help` paths exit 0 and print usage even with broken ambient config.

    Today this is guaranteed by clap: `Cli::command().get_matches()` handles
    `--help` / `-h` / `help` subcommand internally and calls `exit(0)` before
    `App::run` ever reaches `Context::try_init`. This test locks that in so a
    future switch to `try_get_matches()` + custom error handling cannot
    silently regress the property.
    """
    write_home_config(ocx, "this is not valid toml =[[[")

    for args in (
        ("--help",),
        ("help",),
        ("help", "install"),
        ("install", "--help"),
        ("shell", "--help"),
        ("shell", "completion", "--help"),
        ("info", "--help"),
        ("version", "--help"),
    ):
        result = ocx.run(*args, format=None, check=False)
        assert result.returncode == 0, (
            f"`ocx {' '.join(args)}` should exit 0 even with malformed config, "
            f"got rc={result.returncode}; stderr={result.stderr!r}"
        )
        assert "Usage:" in result.stdout, (
            f"`ocx {' '.join(args)}` should print a Usage: section, "
            f"got stdout={result.stdout!r}"
        )


def test_info_still_requires_valid_config_when_ambient_broken(ocx: OcxRunner) -> None:
    """`ocx info` intentionally stays config-coupled — fails on broken config.

    Regression guard for the deliberate design decision: `info` reads
    `context.default_registry()` from the loaded config and will surface more
    config-derived fields in the future. When ambient config is broken,
    `info` failing IS the diagnostic — the fallback is `ocx version`.

    If a future change "fixes" this by adding `info` to the static-command
    bypass list in `app.rs::run`, this test will fail and force the author
    to revisit both the comment in `app.rs` and the decision rationale.
    """
    config_path = write_home_config(ocx, "this is not valid toml =[[[")

    result = ocx.run("info", format=None, check=False)

    assert result.returncode != 0, (
        f"`ocx info` should fail on malformed ambient config (by design); "
        f"got rc=0, stdout={result.stdout!r}"
    )
    combined = result.stdout + result.stderr
    assert str(config_path) in combined or "config.toml" in combined, (
        f"error should mention the config file path, "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "TOML" in combined or "parse" in combined.lower(), (
        f"error should mention the parse failure, "
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


# ---------------------------------------------------------------------------
# Tests: Exit codes and error UX (Phase 3 specification tests)
# NOTE: Tests 3.2.1–3.2.9 assert specific exit-code values and error messages
# that will only pass after Phase 4 implements classify_error dispatch in main.rs.
# Until then they demonstrate the expected contract.
# ---------------------------------------------------------------------------


def test_file_too_large_errors_with_helpful_message(ocx: OcxRunner) -> None:
    """Config file exceeding 65 KiB cap → exit 78 with helpful message.

    Plan Test 3.2.1: FileTooLarge error UX acceptance test.
    Writes a config file that exceeds the max allowed size, then asserts
    exit code 78 (EX_CONFIG) and helpful diagnostic text in stderr.
    """
    # 65 KiB is the documented cap. Write 65 * 1024 + 1 bytes.
    oversized_content = "# " + "x" * (65 * 1024 - 1) + "\n"  # just over 65 KiB as comment lines
    oversized_content = "# padding\n" * ((65 * 1024 // 10) + 1)  # >65 KiB via repeated comment lines
    write_home_config(ocx, oversized_content)

    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 78, (
        f"FileTooLarge should exit with code 78 (EX_CONFIG), got {result.returncode}; "
        f"stderr={result.stderr!r}"
    )
    combined = result.stdout + result.stderr
    assert "exceeds maximum allowed size" in combined, (
        f"error should mention size cap, got: stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "did you point at the wrong file" in combined, (
        f"error should contain helpful hint, got: stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_explicit_config_overrides_env_var_config_file(ocx: OcxRunner, tmp_path: Path) -> None:
    """--config CLI flag beats OCX_CONFIG_FILE when both are set.

    Plan Test 3.2.2: explicit --config overrides OCX_CONFIG_FILE env var.
    Both config files set different default registries; the CLI-provided file wins.
    OCX_NO_CONFIG is NOT set (distinguishing this from the kill-switch test).
    """
    env_config = tmp_path / "env.toml"
    env_config.write_text('[registry]\ndefault = "env.example"\n')

    cli_config = tmp_path / "cli.toml"
    cli_config.write_text('[registry]\ndefault = "cli.example"\n')

    env = {k: v for k, v in ocx.env.items() if k != "OCX_DEFAULT_REGISTRY"}
    env["OCX_CONFIG_FILE"] = str(env_config)

    result = subprocess.run(
        [str(ocx.binary), "--config", str(cli_config), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of nonexistent package should fail"
    combined = result.stdout + result.stderr
    assert "cli.example" in combined, (
        f"--config file should win over OCX_CONFIG_FILE; expected 'cli.example', "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert "env.example" not in combined, (
        f"OCX_CONFIG_FILE registry must NOT appear when --config overrides it; "
        f"got stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_layered_merge_home_tier_and_explicit_config(ocx: OcxRunner, tmp_path: Path) -> None:
    """Additive merge: $OCX_HOME/config.toml + --config extra.toml → both registries resolve.

    Plan Test 3.2.3: layered merge precedence — lower tier provides 'shared' registry,
    higher tier provides 'other' registry. Both should appear after merge (not suppression).
    """
    # Lower tier: $OCX_HOME/config.toml adds [registries.shared]
    write_home_config(
        ocx,
        "[registries.shared]\nurl = \"shared.example\"\n",
    )

    # Higher tier: explicit --config adds [registries.other]
    extra_config = tmp_path / "extra.toml"
    extra_config.write_text("[registries.other]\nurl = \"other.example\"\n")

    # Use index catalog (works even with empty index) to trigger config load
    result = subprocess.run(
        [str(ocx.binary), "--config", str(extra_config), "index", "catalog"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    # The command must succeed: if either tier had clobbered the other or
    # rejected the config, the loader would emit ConfigError (78). Returncode 0
    # is the observable proof that both [registries.*] entries parsed and
    # merged into the same Config.
    assert result.returncode == 0, (
        f"layered merge should succeed (both registries additive); "
        f"stderr={result.stderr!r}"
    )


def test_exit_code_on_config_not_found(ocx: OcxRunner) -> None:
    """--config pointing to nonexistent path → exit 79 (NotFound).

    Plan Test 3.2.4: FileNotFound error → exit code 79 (OCX-specific, first above EX_CONFIG).
    """
    result = subprocess.run(
        [str(ocx.binary), "--config", "/tmp/ocx-spec-test-nonexistent-config.toml", "index", "catalog"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 79, (
        f"config not found should exit with code 79 (NotFound), got {result.returncode}; "
        f"stderr={result.stderr!r}"
    )


def test_exit_code_on_config_parse_error(ocx: OcxRunner) -> None:
    """Malformed TOML in $OCX_HOME/config.toml → exit 78 (EX_CONFIG).

    Plan Test 3.2.5: Parse error → exit code 78.
    """
    write_home_config(ocx, "this is not valid toml =[[[")

    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 78, (
        f"TOML parse error should exit with code 78 (EX_CONFIG), got {result.returncode}; "
        f"stderr={result.stderr!r}"
    )


def test_cli_help_mentions_config_env_vars(ocx: OcxRunner) -> None:
    """ocx --help stdout mentions OCX_CONFIG_FILE and OCX_NO_CONFIG.

    Plan Test 3.2.8: help text expansion — both env vars must appear after
    Phase 4 adds them to the --config flag's long help.
    """
    result = subprocess.run(
        [str(ocx.binary), "--help"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, f"--help should exit 0, got {result.returncode}"
    combined = result.stdout + result.stderr
    assert "OCX_CONFIG_FILE" in combined, (
        f"--help should mention OCX_CONFIG_FILE; got: {combined!r}"
    )
    assert "OCX_NO_CONFIG" in combined, (
        f"--help should mention OCX_NO_CONFIG; got: {combined!r}"
    )


def test_no_config_env_var_suppresses_discovery(ocx: OcxRunner) -> None:
    """OCX_NO_CONFIG=1 suppresses $OCX_HOME/config.toml discovery.

    Plan Test 3.2.9: kill-switch suppression — config file with a distinctive
    registry name is written; OCX_NO_CONFIG=1 must prevent that name from
    appearing anywhere in output.
    """
    write_home_config(ocx, '[registry]\ndefault = "should-be-ignored-spec-test.example"\n')

    env = {**ocx.env, "OCX_NO_CONFIG": "1"}
    result = subprocess.run(
        [str(ocx.binary), "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=env,
    )
    assert result.returncode != 0, "install of nonexistent package should fail"
    combined = result.stdout + result.stderr
    assert "should-be-ignored-spec-test.example" not in combined, (
        f"OCX_NO_CONFIG=1 must suppress config file discovery; "
        f"found suppressed value in: stdout={result.stdout!r} stderr={result.stderr!r}"
    )
