# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx init`` (Unit 7 — specification mode).

Tests encode the contract for the ``ocx init`` command before the
implementation lands. Every test is expected to FAIL against the current
stub (``unimplemented!("Unit 7 — feat(cli): ocx init")``).

Spec source: plan ``auto-findings-md-eventual-fox.md`` Unit 7 §1.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

from src.runner import OcxRunner

# Exit codes per quality-rust-exit_codes.md / error.rs ClassifyExitCode:
# ConfigAlreadyExists → UsageError = 64
EXIT_SUCCESS = 0
EXIT_USAGE_ERROR = 64


def _run_init(
    ocx: OcxRunner,
    cwd: Path,
    *extra: str,
    root: tuple[str, ...] = (),
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), *root, "init", *extra]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env, check=False,
    )


def test_init_creates_minimal_ocx_toml(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init`` in an empty directory creates ``ocx.toml`` with a ``[tools]``
    table and exits 0.

    Spec: Unit 7 §1 bullet 1.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    result = _run_init(ocx, project_dir)

    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init should exit 0; rc={result.returncode}, stderr={result.stderr!r}"
    )
    toml_path = project_dir / "ocx.toml"
    assert toml_path.exists(), "ocx.toml must be created by ocx init"
    content = toml_path.read_text()
    assert "[tools]" in content, (
        f"ocx.toml must contain a [tools] table; got:\n{content}"
    )


def test_init_idempotent_error_when_file_exists(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init`` in a directory that already has ``ocx.toml`` exits with
    UsageError (64) and does NOT overwrite the existing file.

    Spec: Unit 7 §1 bullet 2. Error variant: ``ConfigAlreadyExists`` →
    ``UsageError`` (64) per ``error.rs::ClassifyExitCode``.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    toml_path = project_dir / "ocx.toml"
    original_content = "# sentinel — must not be overwritten\n[tools]\n"
    toml_path.write_text(original_content)

    result = _run_init(ocx, project_dir)

    assert result.returncode == EXIT_USAGE_ERROR, (
        f"ocx init on existing ocx.toml must exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    # File must not be overwritten.
    assert toml_path.read_text() == original_content, (
        "ocx init must not overwrite an existing ocx.toml"
    )


def test_init_advertises_no_key_the_parser_rejects(ocx: OcxRunner, tmp_path: Path) -> None:
    """The generated template contains no commented-out ``registry`` key.

    ``ocx.toml`` has no such field and its parser is ``deny_unknown_fields``, so
    the hint the template used to carry broke the file for anyone who took it up
    on the offer. The default registry lives in ``config.toml``
    (``[registry] default``), not in the project manifest.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    result = _run_init(ocx, project_dir)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    assert "registry" not in content, (
        f"ocx init must not advertise a `registry` key in ocx.toml; got:\n{content}"
    )


def test_init_emits_schema_directive_on_first_line(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init`` writes a ``#:schema https://ocx.sh/schemas/project/v1.json``
    directive on the first line so taplo / VS Code / Zed pick up the
    canonical project schema for autocompletion + validation without
    additional configuration.

    Cluster D.4 — schema discoverability via the standard taplo
    ``#:schema URL`` form. Anchored to the canonical published URL so a
    drift in either source surfaces here.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    result = _run_init(ocx, project_dir)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    first_line = content.splitlines()[0] if content else ""
    expected = "#:schema https://ocx.sh/schemas/project/v1.json"
    assert first_line == expected, (
        f"ocx init must emit the schema directive on the first line; "
        f"expected {expected!r}, got {first_line!r}\nfull content:\n{content}"
    )


def test_init_minimal_content_matches_research(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init`` writes non-interactive minimal content: a registry
    declaration and an empty ``[tools]`` table, at most 10 non-blank lines.

    Spec: Unit 7 §1 bullet 3. Design reference:
    ``.claude/artifacts/research_cli_package_manager_conventions.md`` §6.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    result = _run_init(ocx, project_dir)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    non_blank_lines = [ln for ln in content.splitlines() if ln.strip()]
    assert len(non_blank_lines) <= 10, (
        f"ocx init output should be minimal (≤10 non-blank lines); "
        f"got {len(non_blank_lines)}:\n{content}"
    )
    # Must have [tools] so `ocx add` can append entries.
    assert "[tools]" in content, "ocx.toml must contain [tools] table"
    # Must not contain interactive prompts or questionnaire artifacts.
    lower = content.lower()
    for questionnaire_token in ("enter", "please", "y/n", "yes/no", "(y)", "(n)"):
        assert questionnaire_token not in lower, (
            f"ocx init must be non-interactive; found {questionnaire_token!r} in output:\n{content}"
        )


def _consent_stamps(ocx_home: Path) -> list[Path]:
    """Every consent stamp under this run's isolated ``$OCX_HOME``.

    Globbed rather than keyed: the home is per-test, so the set is the answer to
    "did init stamp anything", and the stamp's own ``project_dir`` field carries
    the identity assertion without re-deriving ``ReferenceManager::name_for_path``
    here (which would make the test agree with itself instead of the binary).
    """
    return sorted((ocx_home / "state" / "projects").glob("*/consent.json"))


def test_init_records_a_consent_stamp(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init`` consents to the project it creates (ocx-sh/ocx#397).

    Creating an ``ocx.toml`` in a directory is at least as deliberate a gesture
    as the ``ocx add`` that already stamps one, and without the stamp the very
    next prompt reports the project just created as inert. The source set is
    empty because there is no lock yet — the first ``ocx add`` re-records it.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    assert _consent_stamps(ocx.ocx_home) == [], "no stamp may exist before ocx init"

    result = _run_init(ocx, project_dir)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    stamps = _consent_stamps(ocx.ocx_home)
    assert len(stamps) == 1, f"ocx init must write exactly one consent stamp; got {stamps}"
    stamp = json.loads(stamps[0].read_text())
    assert Path(stamp["project_dir"]) == project_dir.resolve(), (
        f"the stamp must name the initialised project; got {stamp['project_dir']!r}"
    )
    assert stamp["sources"] == [], (
        f"a project with no lock resolves from no source; got {stamp['sources']!r}"
    )


def test_init_no_consent_writes_no_stamp(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init --no-consent`` creates the project without consenting to it."""
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    result = _run_init(ocx, project_dir, "--no-consent")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init --no-consent failed: rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert (project_dir / "ocx.toml").exists(), "--no-consent must still create ocx.toml"
    assert _consent_stamps(ocx.ocx_home) == [], (
        "--no-consent must write no consent stamp"
    )


def test_init_with_no_consent_env_writes_no_stamp(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``OCX_NO_CONSENT=1 ocx init`` scaffolds without consenting (ocx-sh/ocx#400).

    ``ocx init`` is the one allowlist member that reaches the write seam by a
    different route than the six mutators, so the env var has to be proven
    here separately — a gate that covered only the shared wrapper would leave
    this command stamping.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    result = _run_init(ocx, project_dir, extra_env={"OCX_NO_CONSENT": "1"})
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert (project_dir / "ocx.toml").exists(), (
        "OCX_NO_CONSENT must still create ocx.toml"
    )
    assert _consent_stamps(ocx.ocx_home) == [], (
        "OCX_NO_CONSENT=1 must write no consent stamp"
    )


def test_init_consent_flag_outranks_the_env_var(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``OCX_NO_CONSENT=1 ocx init --consent`` stamps anyway.

    The ladder is flag, then env, then stamp. A resolution that read the env
    before the flag would pass the test above and fail only here.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    result = _run_init(
        ocx, project_dir, "--consent", extra_env={"OCX_NO_CONSENT": "1"}
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init --consent failed: rc={result.returncode}, stderr={result.stderr!r}"
    )
    stamps = _consent_stamps(ocx.ocx_home)
    assert len(stamps) == 1, (
        f"--consent must outrank OCX_NO_CONSENT and stamp; got {stamps}"
    )


def test_init_global_writes_the_ocx_home_manifest(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --global init`` scaffolds ``$OCX_HOME/ocx.toml`` (ocx-sh/ocx#443).

    ``--global`` is a root flag that parses on every subcommand, so an ``init``
    that ignored it wrote ``<cwd>/ocx.toml`` while ``--global add`` and
    ``--global status`` operated on ``$OCX_HOME/ocx.toml`` — the reported
    inconsistency. The CWD assertion is the half that reds on a regression: a
    resolver that walks or falls back to the process directory satisfies the
    first assertion by accident only if ``$OCX_HOME`` happens to be the CWD,
    which it is not here.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    global_manifest = ocx.ocx_home / "ocx.toml"
    assert not global_manifest.exists(), "the isolated $OCX_HOME must start without a manifest"

    result = _run_init(ocx, project_dir, root=("--global",))

    assert result.returncode == EXIT_SUCCESS, (
        f"ocx --global init should exit 0; rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert global_manifest.exists(), (
        "ocx --global init must create $OCX_HOME/ocx.toml"
    )
    assert "[tools]" in global_manifest.read_text(), (
        "the global manifest must carry the same [tools] scaffold as a project one"
    )
    assert not (project_dir / "ocx.toml").exists(), (
        "ocx --global init must not scaffold the current directory"
    )
    # `ui().success` writes to stderr — stdout stays free for machine output.
    assert str(global_manifest) in result.stderr, (
        f"the success line must name the global manifest; got {result.stderr!r}"
    )
    # A-44: a project that *is* $OCX_HOME needs no activation stamp.
    assert _consent_stamps(ocx.ocx_home) == [], (
        "an $OCX_HOME project takes no consent stamp (A-44)"
    )


def test_init_global_refuses_an_existing_global_manifest(ocx: OcxRunner, tmp_path: Path) -> None:
    """A second ``ocx --global init`` exits 64 and leaves the manifest intact.

    Same ``ConfigAlreadyExists`` contract as the project tier — the global
    branch must not become a silent overwrite of a populated toolchain.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    global_manifest = ocx.ocx_home / "ocx.toml"
    global_manifest.parent.mkdir(parents=True, exist_ok=True)
    original_content = "# sentinel — must not be overwritten\n[tools]\n"
    global_manifest.write_text(original_content)

    result = _run_init(ocx, project_dir, root=("--global",))

    assert result.returncode == EXIT_USAGE_ERROR, (
        f"ocx --global init on an existing manifest must exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert global_manifest.read_text() == original_content, (
        "ocx --global init must not overwrite an existing $OCX_HOME/ocx.toml"
    )
