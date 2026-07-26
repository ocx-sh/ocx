"""Tests for OCI-tier per-package env (``ocx package env``).

Rewritten Phase 5 (plan_toolchain_cli.md):
- ``ocx env <pkg>`` → ``ocx package env <pkg>`` (OCI-tier, C3 contract).
- ``ocx shell env <pkg>`` (deleted) → rewritten to assert exit 64.

Note (W5): ``ocx package env`` auto-installs missing packages via
``find_or_install_all`` (deliberate, handshake §2 accepted this cut).
Do NOT assert old 'shell env no-download' semantics against ``package env``.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

from src import OcxRunner, PackageInfo, registry_dir
from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest

# Exit code for deleted commands
EXIT_USAGE = 64


def test_env_path_contains_bin(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package install <pkg>; ocx package env <pkg> → PATH includes /bin"""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)
    path_entry = next(e for e in env_result["entries"] if e["key"] == "PATH")
    assert "/bin" in path_entry["value"] or "\\bin" in path_entry["value"]


def test_env_constant_contains_content_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package install <pkg>; ocx package env <pkg> — constant var points to content dir"""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)

    home_key = pkg.repo.upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("package", "env", pkg.short)
    home_entry = next(e for e in env_result["entries"] if e["key"] == home_key)
    assert registry_dir(ocx.registry) in home_entry["value"]
    # CAS layout: packages/{registry}/sha256/{prefix}/{suffix}/content
    assert "packages" in home_entry["value"]


def test_env_candidate_uses_symlink_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx package install <pkg>; ocx package env --candidate <pkg>"""
    pkg = published_package
    ocx.plain("package", "install", pkg.short)

    home_key = pkg.repo.upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("package", "env", "--candidate", pkg.short)
    home_entry = next(e for e in env_result["entries"] if e["key"] == home_key)
    assert f"candidates/{pkg.tag}" in home_entry["value"] or f"candidates\\{pkg.tag}" in home_entry["value"]


def test_shell_env_removed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """``ocx shell env <pkg>`` → exit 64 (deleted command, plan C4).

    The ``ocx shell env`` command is removed. Per-package env is now
    ``ocx package env``; sourceable form uses ``--shell[=NAME]``.
    """
    pkg = published_package
    result = subprocess.run(
        [str(ocx.binary), "shell", "env", pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == EXIT_USAGE, (
        f"ocx shell env must exit {EXIT_USAGE} (deleted); "
        f"got {result.returncode}\nstderr:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# adr_declared_binaries_metadata.md §4 — `binaries`/`entrypoints` JSON arrays
# ---------------------------------------------------------------------------


def test_package_env_json_binaries_array_has_package_attribution(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx package env`'s JSON `binaries` array names the admitted package
    that declared each claim (`package` = the admitted `PinnedIdentifier`'s
    string form, ADR §4 Decision A)."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)

    digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    assert env_result["binaries"] == [{"name": "hello", "package": f"{pkg.fq}@{digest}"}], (
        env_result["binaries"]
    )
    assert env_result["entrypoints"] == [], env_result["entrypoints"]


def test_package_env_json_binaries_entrypoints_present_but_empty_without_claims(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`binaries`/`entrypoints` are always present as arrays — possibly
    empty — even for a package that declares neither (ADR §4).

    `--no-bin-scan` keeps the claim genuinely absent despite an
    interface-visible executable in `bin/` — Auto mode (the default) would
    otherwise fill it, which is exactly the behavior under test elsewhere.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], no_bin_scan=True)
    ocx.plain("package", "install", pkg.short)

    env_result = ocx.json("package", "env", pkg.short)

    assert env_result["binaries"] == []
    assert env_result["entrypoints"] == []


def test_package_env_shell_output_excludes_binaries_and_entrypoints(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--shell` stays the eval-safe channel only — a declared `binaries`
    claim never leaks into it (ADR §4: both sinks return before `EnvVars`
    is even constructed)."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    result = ocx.plain("package", "env", pkg.short, "--shell=bash")

    assert result.returncode == 0, result.stderr
    assert "export" in result.stdout, result.stdout
    assert "binaries" not in result.stdout.lower(), result.stdout
    assert "hello" not in result.stdout, result.stdout


def test_package_env_ci_github_output_excludes_binaries_and_entrypoints(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--ci=github`` stays a CI persistence sink only — a declared `binaries`
    claim never leaks into the ``$GITHUB_ENV`` / ``$GITHUB_PATH`` sink files
    (ADR §4: both sinks return before `EnvVars` is even constructed)."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    github_path = tmp_path / "github_path"
    github_env = tmp_path / "github_env"
    github_path.write_text("")
    github_env.write_text("")

    result = ocx.plain(
        "package",
        "env",
        pkg.short,
        "--ci=github",
        env_overrides={
            "GITHUB_ACTIONS": "true",
            "GITHUB_PATH": str(github_path),
            "GITHUB_ENV": str(github_env),
        },
    )

    assert result.returncode == 0, result.stderr
    sink_text = github_path.read_text() + github_env.read_text()
    assert "binaries" not in sink_text.lower(), sink_text
    assert "hello" not in sink_text, sink_text


def test_package_env_plain_shows_hint_when_binaries_present(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Decision C: the plain `entries` table stays byte-stable; a hint line
    below it announces binary/entrypoint availability when the admitted set
    carries any claims."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["hello"])
    ocx.plain("package", "install", pkg.short)

    result = ocx.plain("package", "env", pkg.short)

    assert result.returncode == 0, result.stderr
    assert "hello" in result.stdout, result.stdout
    assert "available" in result.stdout.lower(), result.stdout


def test_package_env_plain_omits_hint_without_claims(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """No hint line renders when the admitted set carries no binaries or
    entrypoints (the common case, unaffected by this feature).

    `--no-bin-scan` keeps the claim genuinely absent despite an
    interface-visible executable in `bin/`.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], no_bin_scan=True)
    ocx.plain("package", "install", pkg.short)

    result = ocx.plain("package", "env", pkg.short)

    assert result.returncode == 0, result.stderr
    assert "available" not in result.stdout.lower(), result.stdout


# ---------------------------------------------------------------------------
# `--env` on the OCI tier
#
# `--env` is a per-invocation CLI override, not project configuration, so it
# lands here too. It does NOT make this tier read `ocx.toml` — there is no
# `-g`, no project `[env]`, nothing but what the caller typed.
# ---------------------------------------------------------------------------


def _export_value(lines: str, key: str) -> str | None:
    """Return the effective value of ``export KEY="value"`` in a shell block.

    Returns the LAST assignment: the block replays the composed entry vector,
    so a later-stage constant is emitted as a second ``export`` line and a
    shell evaluating top to bottom keeps the last one.
    """
    prefix = f"export {key}="
    effective: str | None = None
    for line in lines.splitlines():
        if not line.startswith(prefix):
            continue
        value = line[len(prefix) :]
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        effective = value
    return effective


def _dumped_value(dump: str, key: str) -> str | None:
    """Return the value of a ``KEY=value`` line in an ``env``-dumped block."""
    prefix = f"{key}="
    for line in dump.splitlines():
        if line.startswith(prefix):
            return line[len(prefix) :]
    return None


def test_package_env_flag_matches_what_package_exec_executes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx package env --env`` prints what ``ocx package exec --env`` runs with.

    The export/execute parity that justifies the flag existing on an emit-only
    command, asserted against one shared oracle so a divergence between the two
    fails rather than two independently-passing assertions.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"])
    ocx.plain("package", "install", pkg.short)

    overrides = ["--env", "FROM_FLAG=flag-value"]

    executed = subprocess.run(
        [str(ocx.binary), "package", "exec", *overrides, pkg.short, "--", "env"],
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert executed.returncode == 0, executed.stderr

    exported = subprocess.run(
        [str(ocx.binary), "package", "env", *overrides, "--shell=bash", pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert exported.returncode == 0, exported.stderr

    assert _dumped_value(executed.stdout, "FROM_FLAG") == "flag-value", (
        f"package exec --env must apply the override; stdout:\n{executed.stdout}"
    )
    assert _export_value(exported.stdout, "FROM_FLAG") == "flag-value", (
        f"package env --env must export the same value package exec applies; "
        f"export lines:\n{exported.stdout}"
    )


def test_package_env_flag_does_not_read_ocx_toml(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The OCI tier stays OCI-tier: an `ocx.toml` beside the invocation
    contributes nothing, even though `--env` now composes here.

    Pins the boundary the flag deliberately does not cross.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"])
    ocx.plain("package", "install", pkg.short)

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text('[tools]\n\n[env]\nFROM_FILE = "must-not-appear"\n')

    result = subprocess.run(
        [str(ocx.binary), "package", "exec", "--env", "FROM_FLAG=ok", pkg.short, "--", "env"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert _dumped_value(result.stdout, "FROM_FLAG") == "ok"
    assert _dumped_value(result.stdout, "FROM_FILE") is None, (
        f"the OCI tier must never read ocx.toml; stdout:\n{result.stdout}"
    )


def test_package_exec_env_flag_survives_generated_entrypoint_launcher(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A `--env` override reaches a tool invoked THROUGH a generated launcher.

    A package that declares entrypoints resolves through its launcher, which
    re-enters `ocx launcher exec` — a process with no project context that
    rebuilds its env from scratch and re-applies the package's own entries on
    top. Without `set_forwarded_env` on this command the override is silently
    reverted at that hop.

    The override MUST target a key the package itself declares. A key the
    package does not declare survives the hop by plain inheritance whether or
    not it was forwarded, so a test using one passes either way and proves
    nothing — that is exactly the shape this assertion avoids.
    """
    from src.helpers import make_package_with_entrypoints

    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints={"showenv": {"command": "env"}},
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
            },
            {
                "key": "LAUNCHER_PROBE",
                "type": "constant",
                "value": "package-value",
            },
        ],
    )
    ocx.plain("package", "install", pkg.short)

    result = subprocess.run(
        [str(ocx.binary), "package", "exec", "--env", "LAUNCHER_PROBE=flag-value", pkg.short, "--", "showenv"],
        capture_output=True,
        text=True,
        env=ocx.env,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert _dumped_value(result.stdout, "LAUNCHER_PROBE") == "flag-value", (
        f"the override must survive the launcher re-entry — without forwarding "
        f"the launcher re-applies the package's own 'package-value' on top; "
        f"stdout:\n{result.stdout}"
    )


def test_package_tier_env_flag_rejects_reserved_and_bad_type(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The key and type gates apply identically on this tier — the flag is one
    parser, so `OCX_*` cannot be set and an unknown `:TYPE` is exit 64.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"])
    ocx.plain("package", "install", pkg.short)

    for argument in ("OCX_DEFAULT_REGISTRY=evil.example.com", "X:bogus=v", "X:=v"):
        for command in (
            ["package", "exec", "--env", argument, pkg.short, "--", "hello"],
            ["package", "env", "--env", argument, pkg.short],
        ):
            result = subprocess.run(
                [str(ocx.binary), *command],
                capture_output=True,
                text=True,
                env=ocx.env,
                check=False,
            )
            assert result.returncode == EXIT_USAGE, (
                f"--env {argument} on `ocx {' '.join(command[:2])}` must exit "
                f"{EXIT_USAGE}; got {result.returncode}\nstderr:\n{result.stderr}"
            )
