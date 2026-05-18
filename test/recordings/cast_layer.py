"""Cast-layer: opt-in cast generation for doc scripts (Phase 4, website:build only).

This module is the single source of truth for:

- Command rewriting (display → actual repo name substitution).
- ``maybe_record_cast``: the website-build-only entry point that writes a
  ``.cast`` file from a doc script's ``# region cast`` block.

**Caller-provisions convention**: the caller MUST call
``provider.provision(ocx, tmp_path)`` before invoking ``maybe_record_cast``.
``maybe_record_cast`` does NOT call ``provision`` itself — it assumes the
provider is already provisioned and ``provider.display_map()`` returns valid
data.  Calling this function with an unprovisioned provider will produce empty
sanitize/repo maps and the cast will not rewrite repo names.

Design contract reference: design_spec_doc_command_scripts.md
§1.3, §4 (CA1–CA5).

Import-time guarantee (SP0): importing this module performs **zero**
registry I/O.
"""
from __future__ import annotations

import shlex
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from recordings.cast_recorder import CastRecorder
    from src.doc_scripts import DocScriptMeta
    from src.state_providers import StateProvider


# ---------------------------------------------------------------------------
# Command rewrite — single source of truth (shared with test_recordings.py)
# ---------------------------------------------------------------------------


def rewrite_command(cmd: str, repo_map: dict[str, str]) -> str:
    """Replace display-name package refs with actual repo names in OCX args.

    Skips the first word (the ``ocx`` command itself) and only rewrites the
    part before `` -- `` so binary names passed to ``ocx exec … -- <bin>``
    are left untouched.

    This is the single source of truth for command rewriting shared between
    the cast layer and the legacy recordings runner (``test_recordings.py``).

    Args:
        cmd: The command string as it appears in the script (display form).
        repo_map: Mapping ``{display_name: actual_repo}`` derived from
            ``provider.display_map()`` (SP4).

    Returns:
        The command string with display names replaced by actual repo names.
    """
    if not repo_map:
        return cmd
    parts = cmd.split(" -- ", 1)
    ocx_part = parts[0]

    # Skip the command name (first word, always "ocx")
    first_space = ocx_part.find(" ")
    if first_space == -1:
        return cmd
    command_name = ocx_part[:first_space]
    args = ocx_part[first_space:]

    # Longest-first to avoid partial matches (e.g., "nodejs" before "node")
    for display, actual in sorted(repo_map.items(), key=lambda x: -len(x[0])):
        args = args.replace(display, actual)

    result = command_name + args
    if len(parts) > 1:
        return result + " -- " + parts[1]
    return result


def _substitute_command_head(cmd: str, name: str, replacement: str) -> str:
    """Replace the leading command word *name* with *replacement* only.

    Uses the same first-space split as :func:`rewrite_command`: the head
    token (everything before the first space, or the whole string when there
    is no space) is the command name.  It is replaced only when it exactly
    equals *name* — never a blind first-substring replace, so an ``ocx``
    occurring inside a later argument (a ``my-ocx`` repo, a ``.ocx/`` path)
    is left untouched (W2).

    Args:
        cmd: The command string (post repo rewrite).
        name: The expected head command word (always ``"ocx"`` here).
        replacement: The shell-quoted real binary path to substitute.

    Returns:
        *cmd* with the head token replaced when it equals *name*; otherwise
        *cmd* unchanged.
    """
    first_space = cmd.find(" ")
    if first_space == -1:
        head, tail = cmd, ""
    else:
        head, tail = cmd[:first_space], cmd[first_space:]
    if head != name:
        return cmd
    return replacement + tail


# ---------------------------------------------------------------------------
# Cast-layer entry point (CA1–CA5)
# ---------------------------------------------------------------------------


def maybe_record_cast(
    meta: "DocScriptMeta",
    provider: "StateProvider",
    recorder: "CastRecorder",
    casts_dir: Path,
) -> Path | None:
    """Optionally record a ``.cast`` file for a doc script (website:build only).

    **Caller-provisions convention**: the caller MUST call
    ``provider.provision(ocx, tmp_path)`` before invoking this function.
    ``maybe_record_cast`` does NOT call ``provision``.

    Behaviour:

    - CA1: ``meta.cast is False`` → returns ``None``, writes nothing.
    - CA2: ``meta.cast is True`` → records and writes:
        - ``<casts_dir>/<slug>.cast`` when ``meta.doc`` is set — the
          **nested** path with the slug ``/`` preserved as the directory
          separator (same rule as PT2 / ADR Decision D / LDR 2026-05-17;
          the old ``/`` → ``__`` flatten is removed).  Parent directories
          are created by the recorder ``write`` (``mkdir -p``).
        - ``<casts_dir>/<stem>.cast`` when ``meta.doc`` is ``None``
          (demo-only cast, no prose binding).
    - CA3: this function is the website-build-only entry point; it is never
      called by ``run_doc_script`` (the verify-path executor).
    - CA4: repo rewriting derives exclusively from ``provider.display_map()``
      (SP4); ``recordings.setups.SETUPS`` is never accessed.
    - CA5: only lines inside the single ``# region cast`` … ``# endregion cast``
      block are sent to the recorder via ``recorder.run_command()``.  Lines
      outside the region (``set -euo pipefail``, ``$(…)`` captures,
      ``[[ … ]]`` assertions) are never sent — no PTY hang, no test-scaffold
      leakage into the cast.

    Args:
        meta: Parsed doc-script metadata (from ``parse_doc_header``).  Must
            have ``meta.cast_region`` set when ``meta.cast is True`` (the
            parser enforces this via EX9).
        provider: A provisioned ``StateProvider``.  ``display_map()`` must
            return valid ``(sanitize_map, repo_map)`` after ``provision()``.
        recorder: An open ``CastRecorder`` (or compatible fake).  Must support
            ``run_command(display_cmd, actual_cmd, **kwargs)``,
            ``build(title=…)``, and the ``CastRecording`` chain
            (``.strip_progress().sanitize(…).truncate_digests()``
            ``.realign_tables().auto_height(…).write(path)``).
        casts_dir: Directory where the ``.cast`` file is written.  Created
            recursively if absent.

    Returns:
        The ``Path`` of the written ``.cast`` file, or ``None`` when
        ``meta.cast is False``.
    """
    # CA1: cast: false (or absent, which defaults to False)
    if not meta.cast:
        return None

    # CA4: derive sanitize_map and repo_map from provider only (never SETUPS)
    sanitize_map, repo_map = provider.display_map()

    # CA5: extract only the lines inside the cast region
    region_lines = _extract_region_lines(meta)

    # Obtain the ocx binary path for actual command substitution.
    # We derive it from the environment the provider sets up: if the
    # provider has a runner reference we use it; otherwise fall back to
    # the shlex-quoted "ocx" token so the cast layer works with fakes.
    ocx_binary_str = _resolve_ocx_binary(provider)

    # Replay each in-region line through the recorder
    for line in region_lines:
        display_cmd = line
        # Rewrite package display names → actual UUID-prefixed repo names
        actual_cmd = rewrite_command(line, repo_map)
        # Substitute the real binary path for the command-name head token
        # only.  W2: a blind first-substring replace of "ocx" would also
        # rewrite an "ocx" embedded in a later argument (e.g. a repo named
        # ``my-ocx`` or a path ``.ocx/``).  Reuse the same first-space split
        # rewrite_command uses so only the leading command word is replaced.
        actual_cmd = _substitute_command_head(actual_cmd, "ocx", ocx_binary_str)
        recorder.run_command(display_cmd, actual_cmd)

    # CA2: determine cast filename
    cast_path = _cast_path(meta, casts_dir)

    # Build, sanitize and write via the recorder chain
    title = meta.title or meta.path.stem
    (
        recorder.build(title=title)
        .strip_progress()
        .sanitize(sanitize_map)
        .truncate_digests()
        .realign_tables()
        .auto_height()
        .write(cast_path)
    )

    return cast_path


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------


def _extract_region_lines(meta: "DocScriptMeta") -> list[str]:
    """Return the non-blank, non-comment lines inside the ``# region cast`` block.

    Reads ``meta.path`` and slices the lines defined by ``meta.cast_region``
    (1-based inclusive span set by the parser).  Blank lines and comment lines
    (starting with ``#``) within the region are skipped.

    Args:
        meta: Parsed doc-script metadata with ``cast_region`` set.

    Returns:
        List of command strings suitable for PTY replay.
    """
    assert meta.cast_region is not None, (
        "cast_region must be set for cast: true scripts (parser enforces EX9)"
    )
    start, end = meta.cast_region  # 1-based, inclusive
    all_lines = meta.path.read_text().splitlines()
    # Slice: lines[start-1 : end-1] excludes the sentinel lines themselves
    region_body = all_lines[start:end - 1]  # rows *between* the markers
    result: list[str] = []
    for raw in region_body:
        stripped = raw.strip()
        if not stripped:
            continue
        if stripped.startswith("#"):
            continue
        result.append(stripped)
    return result


def _cast_path(meta: "DocScriptMeta", casts_dir: Path) -> Path:
    """Derive the output ``.cast`` path from ``meta`` (CA2).

    LDR 2026-05-17 (nested slug scheme — same as PT2 / ADR Decision D):
    when ``meta.doc`` is set the cast is written at the **nested** path
    ``<casts_dir>/<slug>.cast`` with the slug ``/`` preserved as the
    directory separator (e.g. ``# doc: authoring/package-cascade`` →
    ``casts/authoring/package-cascade.cast``), matching the page
    ``<Terminal src="/casts/authoring/package-cascade.cast">`` reference.
    Parent directories are created by the caller (``write`` does
    ``mkdir -p``).  Without ``meta.doc`` (demo-only cast, no prose
    binding), falls back to ``<stem>.cast`` at the casts root.
    """
    if meta.doc:
        return casts_dir / f"{meta.doc}.cast"
    return casts_dir / f"{meta.path.stem}.cast"


def _resolve_ocx_binary(provider: "StateProvider") -> str:
    """Extract the ``ocx`` binary token from the provider's runner, if available.

    The cast-layer substitutes the real binary path into ``actual_cmd`` so
    the PTY shell runs the correct binary.  For fakes / unit tests the
    provider may have no runner — in that case we return ``"ocx"`` so the
    fake recorder sees a no-op substitution.
    """
    # SetupAdapter and ScenarioAdapter both store the OcxRunner after provision()
    # as ``_ocx``; ScenarioAdapter stores it on ``_instance``.
    runner = getattr(provider, "_ocx", None)
    if runner is None:
        # ScenarioAdapter stores runner on the scenario instance
        instance = getattr(provider, "_instance", None)
        if instance is not None:
            runner = getattr(instance, "ocx", None)
    if runner is not None:
        binary = getattr(runner, "binary", None)
        if binary is not None:
            return shlex.quote(str(binary))
    return "ocx"


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

__all__ = [
    "maybe_record_cast",
    "rewrite_command",
    "_substitute_command_head",
]
