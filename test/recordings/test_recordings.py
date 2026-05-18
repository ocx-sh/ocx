"""Generic test that runs each cast-enabled doc script as a recording.

For each .sh file in doc_scripts/ with cast: true:
1. Provisions the setup environment (publishes packages via the StateProvider)
2. Executes each command through a persistent bash shell in a PTY
3. Sanitizes output (tmp paths, registry, repo names)
4. Writes the .cast file to the website casts directory

Command rewriting (display → actual repo) uses the shared ``rewrite_command``
from ``recordings.cast_layer`` — single source of truth for both the legacy
recordings runner and the Phase-4 cast layer.
"""
from __future__ import annotations

import shlex
from pathlib import Path
from typing import TYPE_CHECKING, TypedDict

import pytest

from src.runner import OcxRunner, registry_dir

from recordings.cast_layer import _cast_path, _substitute_command_head, rewrite_command
from recordings.cast_recorder import CastRecorder

if TYPE_CHECKING:
    from src.doc_scripts import DocScriptMeta
    from src.state_providers import StateProvider


class ScriptFixture(TypedDict):
    """Shape of the ``script`` fixture produced by conftest.py."""

    meta: DocScriptMeta
    commands: list[str]
    path: Path

# Setups whose scripts run in a publisher work directory so that
# `ocx package create` / `package push` can use relative paths like
# `build/`, `metadata.json`. The recorder silently `cd`s to
# ``provider.work_dir`` (SP8) before the first typed command so the cast
# does not leak the long pytest tmp path.
_PUBLISHER_STATES = {"setup:publisher"}


# Python 3.14 warns on forkpty() in a multi-threaded process.  Under
# ``pytest -n auto`` every xdist worker carries an execnet receiver thread,
# so the process is multi-threaded when pexpect.spawn() -> ptyprocess ->
# pty.fork() forks the recording shell.  Our pattern is fork -> immediate
# exec(bash) with the only sibling thread blocked on a socket recv, so the
# documented deadlock cannot be realized here.  Scope the suppression to
# this exact message (not a blanket DeprecationWarning ignore) so genuine
# forkpty misuse elsewhere still surfaces.  See CPython gh-#... pty fork guard.
@pytest.mark.filterwarnings(
    "ignore:This process .* is multi-threaded, use of forkpty\\(\\):DeprecationWarning"
)
def test_record(
    script: "ScriptFixture",
    ocx: OcxRunner,
    ocx_binary: Path,
    recorder: CastRecorder,
    provider: "StateProvider",
    cast_dir: Path,
    registry: str,
    ocx_home: Path,
    tmp_path: Path,
) -> None:
    meta = script["meta"]
    commands = script["commands"]
    title = meta.title if meta.title else script["path"].stem

    # Build the sanitization map from provider + runner-level paths
    registry_slug = registry_dir(registry)
    sanitize_map, repo_map = provider.display_map()
    # Overlay runner-level path replacements (must come after repo_map build
    # so ocx_home / registry strings don't interfere with repo substitution)
    sanitize_map = {
        **sanitize_map,
        str(ocx_home): "~/.ocx",
        registry + "/": "",
        registry_slug + "/": "",
    }

    # Publisher cd-hack: cd to the directory the provider's setup function
    # actually wrote its inputs into (SP8: provider.work_dir).  For the
    # publisher state this is tmp_path/_state (not tmp_path itself), so
    # relative paths like build/ and metadata.json resolve correctly.
    if meta.state in _PUBLISHER_STATES:
        work_dir = provider.work_dir if provider.work_dir is not None else tmp_path
        recorder.silent_setup(f"cd {shlex.quote(str(work_dir))}")
        sanitize_map[str(work_dir) + "/"] = ""
        sanitize_map[str(work_dir)] = ""

    # Inject the StateProvider env projection into the persistent PTY shell so
    # `$PKG_*` / `$REPO_*` / `$SCENARIO_TMP` etc. resolve in replayed commands
    # exactly as they do in the drift-gate subprocess (one script, `$PKG_*`
    # everywhere — converged-tree model).  Skip PATH/OCX/OCX_HOME/REGISTRY:
    # the recorder shell already inherits a consistent set from `ocx.env`
    # (same OcxRunner the provider provisioned into); re-exporting PATH would
    # clobber the recorder's shell PATH.
    _SKIP_ENV = {"PATH", "OCX", "OCX_HOME", "REGISTRY"}
    proj_env = {
        k: v for k, v in provider.script_env().items() if k not in _SKIP_ENV
    }
    if proj_env:
        exports = " ".join(
            f"export {k}={shlex.quote(v)};" for k, v in proj_env.items()
        )
        recorder.silent_setup(exports)

    # Canonical display map (PKG_<KEY> -> clean short, e.g. "webapp:2.0.0") so
    # the *displayed* cast text shows the reader-facing form, not literal
    # `$PKG_WEBAPP`.  Same source the publish render uses (declared_display_env
    # / RN3) — keeps cast and rendered snippet visually consistent.
    declared = provider.declared_display_env()

    # Binary path for substitution into actual commands
    binary_quoted = shlex.quote(str(ocx_binary))

    # Execute each command through the persistent shell
    for cmd in commands:
        # Displayed form: expand $PKG_<KEY>/${PKG_<KEY>} (quoted or not) to the
        # canonical short via declared_display_env, then apply sanitize_map for
        # any residual actual-repo strings.
        display_cmd = cmd
        for var, val in declared.items():
            for tok in (f'"${{{var}}}"', f'"${var}"', f"${{{var}}}", f"${var}"):
                display_cmd = display_cmd.replace(tok, val)
        for old, new in sanitize_map.items():
            display_cmd = display_cmd.replace(old, new)

        # Executed form: the PTY shell now has $PKG_* exported (resolves to the
        # SP7-prefixed actual repo via script_env), so the literal command runs
        # as-is; rewrite_command still maps any bare display-name literals, and
        # the first `ocx` token is replaced with the real binary path.
        actual_cmd = rewrite_command(cmd, repo_map)
        actual_cmd = _substitute_command_head(actual_cmd, "ocx", binary_quoted)

        recorder.run_command(display_cmd, actual_cmd, timeout=120)
        recorder.pause(0.5)

    # Build, sanitize, truncate digests, and write.
    # CA2 (LDR 2026-05-17): cast written at the NESTED slug path
    # <cast_dir>/<slug>.cast (slug `/` = dir separator), matching the
    # website <Terminal src="/casts/<slug>.cast"> reference and the publish
    # nested scheme.  Falls back to path stem when # doc: is absent.
    cast_output = _cast_path(meta, cast_dir)
    (
        recorder.build(title=title)
        .strip_progress()
        .sanitize(sanitize_map)
        .truncate_digests()
        .realign_tables()
        .auto_height()
        .write(cast_output)
    )


# ---------------------------------------------------------------------------
# Unit-level regression tests for _substitute_command_head (W2)
# ---------------------------------------------------------------------------
# These tests exercise the substitution helper directly — no PTY, no registry.
# They guard against the W2 regression where a bare .replace("ocx", …, 1)
# rewrote the first substring occurrence of "ocx" rather than the leading
# command token, corrupting commands that contain "ocx" in a later argument
# (e.g. a repo named "my-ocx" or a path ".ocx/…").


def test_substitute_command_head_rewrites_leading_token() -> None:
    """Head token 'ocx' is replaced with the real binary path."""
    result = _substitute_command_head("ocx install webapp:1.0.0", "ocx", "/usr/local/bin/ocx")
    assert result == "/usr/local/bin/ocx install webapp:1.0.0"


def test_substitute_command_head_leaves_later_ocx_in_arg_untouched() -> None:
    """W2 regression: 'ocx' inside a later argument must NOT be rewritten.

    A command like 'ocx index update my-ocx' must not have 'my-ocx' corrupted
    by the substitution — only the leading 'ocx' command token is replaced.
    """
    result = _substitute_command_head("ocx index update my-ocx", "ocx", "/usr/bin/ocx")
    assert result == "/usr/bin/ocx index update my-ocx"


def test_substitute_command_head_leaves_dot_ocx_path_untouched() -> None:
    """W2 regression: '.ocx/' path in a later argument must NOT be rewritten."""
    result = _substitute_command_head("ocx install --home .ocx/store webapp:1", "ocx", "/bin/ocx")
    assert result == "/bin/ocx install --home .ocx/store webapp:1"


def test_substitute_command_head_no_op_when_head_differs() -> None:
    """When the head token is not 'ocx', the command is returned unchanged."""
    result = _substitute_command_head("bash -c 'ocx install foo'", "ocx", "/bin/ocx")
    assert result == "bash -c 'ocx install foo'"


def test_substitute_command_head_single_word_command() -> None:
    """A bare 'ocx' with no arguments is replaced correctly."""
    result = _substitute_command_head("ocx", "ocx", "/bin/ocx")
    assert result == "/bin/ocx"
