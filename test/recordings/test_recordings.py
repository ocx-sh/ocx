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

from src.doc_scripts import strip_ansi
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


# ---------------------------------------------------------------------------
# Live-reconciler gate (ocx-sh/ocx#351)
# ---------------------------------------------------------------------------

# Where `self activate` looks for the binary it bakes into the emitted hook
# body as `if [ -x '<this>' ]` — resolved from `$OCX_HOME`, never from `$PATH`
# (`activate.rs::ocx_install_bin_path`).  A recorder home without this file
# registers the hook, fires it every prompt, and reconciles nothing.
_INSTALL_BIN_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin"

# `api/data/shell_state.rs`'s wording for "consented, but not applied yet".
# Every `ocx` command in a cast is reached through a fresh prompt, so a live
# hook has already applied the scope before the next command is typed — this
# line can only survive into a recording whose hook is inert.
_UNKEPT_PROMISE = "the next prompt applies it"


def _assert_reconciler_was_alive(
    recorder: CastRecorder,
    commands: list[str],
    transcript: list[str],
    path: Path,
    probe: Path,
) -> None:
    """Fail a cast that activates the shell hook and then records it doing nothing.

    Two guards, because either alone has a blind spot:

    - ``__ocx_pwd`` is written **only** by ``hook::checkpoint``, which rides a
      reconcile's own output.  Nothing seeds it at shell start — unlike
      ``__OCX_ENV_STATE``, which ``activate.rs::seed_carrier`` exports before any
      prompt has run, and which is therefore not evidence of anything.  Non-empty
      is proof the hook body *executed*, not merely that it was registered.
      Probed through the same shell after the replay, so it measures the recorded
      session rather than a fresh one.
    - The transcript must not leave a promised apply unfulfilled.  This is the
      shape of #351 itself, and it also catches a cast that reconciles once at
      shell start and then goes stale for the rest of the recording.
    """
    if not any("self activate" in command for command in commands):
        return

    recorder.silent_setup(f'printf %s "${{__ocx_pwd-}}" >{shlex.quote(str(probe))}')
    recorded_pwd = probe.read_text(encoding="utf-8") if probe.is_file() else ""
    assert recorded_pwd, (
        f"{path.name}: the cast evals `ocx self activate`, but the recorder shell's "
        "`__ocx_pwd` is unset — `__ocx_prompt_hook` never reached `hook::checkpoint`, "
        "so every prompt in this recording reconciled nothing and the cast documents "
        "the feature's absence (ocx-sh/ocx#351). First thing to check: whether "
        f"`$OCX_HOME/{_INSTALL_BIN_REL}/ocx` exists, since the emitted hook body "
        "guards its whole reconcile on `[ -x … ]` against exactly that path."
    )

    unkept = [output for output in transcript if _UNKEPT_PROMISE in strip_ansi(output)]
    assert not unkept, (
        f"{path.name}: a recorded command reports {_UNKEPT_PROMISE!r}, but the "
        "reader's next prompt in this cast has already come and gone without "
        "applying anything. Re-record so the cast shows the apply (ocx-sh/ocx#351).\n"
        + "\n---\n".join(strip_ansi(output) for output in unkept)
    )


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

    # Replay the scenario working directory the scripts themselves establish
    # with `cd "$SCENARIO_TMP"` — that prelude sits outside the cast region
    # (CA5) and is therefore never typed, so without this the shell would keep
    # pytest's cwd (the repo's `test/`) and project-scoped commands would find
    # the repository's own ocx.toml instead of the scenario's.  For publisher
    # states the provider's work dir is tmp_path/_state (SP8), which is where
    # relative paths like build/ and metadata.json resolve.  The cd is silent
    # and the path is sanitized so the cast never leaks the pytest tmp path.
    #
    # The work dir renders as `~`, the same fiction `$OCX_HOME` -> `~/.ocx`
    # already uses, so a path *under* it reads as `~/demo` rather than a bare
    # `demo`.  It must not render as the empty string: `shell_state.rs` prints
    # the project directory as a value of its own, and blanking it produced
    # `project: ""` — a field the reader cannot tell from a missing one.
    work_dir = provider.work_dir if provider.work_dir is not None else tmp_path
    recorder.silent_setup(f"cd {shlex.quote(str(work_dir))}")
    sanitize_map[str(work_dir)] = "~"

    # Give the recorder shell an *installed* ocx, which is what every reader of
    # a cast has and what `self activate` assumes.  Two things need it: the
    # emitted hook body guards its whole reconcile on
    # `[ -x '$OCX_HOME/symlinks/…/bin/ocx' ]`, and a bare `ocx` inside a command
    # substitution — `eval "$(ocx self activate --shell=bash)"` — is not
    # rewritten by `_substitute_command_head`, which only ever replaces a
    # command *head*.  Without this the eval ran whichever ocx the host happened
    # to have on `$PATH` (none, in CI) and installed a hook pointing at a binary
    # that does not exist, so every prompt reconciled nothing (#351).
    install_bin = ocx_home / _INSTALL_BIN_REL
    install_bin.mkdir(parents=True, exist_ok=True)
    (install_bin / ocx_binary.name).symlink_to(ocx_binary)
    recorder.silent_setup(f"export PATH={shlex.quote(str(install_bin))}:$PATH")

    # Same problem for a third-party tool the script drives as itself (the
    # cosign-parity cast types a bare `cosign …`): the provider materialised it
    # during provision(), but this shell was spawned from `ocx.env` before that
    # ran and the projection below drops PATH. Silent, because a reader has the
    # tool installed the normal way — the cast must not teach them ours.
    for extra_bin in provider.extra_bin_dirs():
        recorder.silent_setup(f"export PATH={shlex.quote(str(extra_bin))}:$PATH")

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
    # `$PKG_ACME_WEBAPP`.  Same source the publish render uses (declared_display_env
    # / RN3) — keeps cast and rendered snippet visually consistent.
    declared = provider.declared_display_env()

    # Binary path for substitution into actual commands
    binary_quoted = shlex.quote(str(ocx_binary))

    # Execute each command through the persistent shell
    transcript: list[str] = []
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

        transcript.append(recorder.run_command(display_cmd, actual_cmd, timeout=120))
        recorder.pause(0.5)

    _assert_reconciler_was_alive(
        recorder, commands, transcript, script["path"], tmp_path / "_hook_probe"
    )

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
