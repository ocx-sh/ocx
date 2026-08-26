# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Shared, stdlib-only helpers for the all-shell per-prompt reconciler matrix.

Consumed by ``test/tests/test_shell_reconcile.py`` (WP-14) and, later, by
``test/tests/test_shell_reconcile_edge_cases.py`` (WP-15). Deliberately
**stdlib + no project imports**: the same file is bind-mounted into the shell-zoo
container at ``/work/shell_matrix.py`` and imported as a top-level module there,
where ``test/src/`` does not exist. Importing ``src.*`` from here would work on
the host and break silently in the zoo.

Three things live here, and nothing else:

1. :data:`ARMS` — the nine shell arms, each mapping an ``ocx --shell=<name>``
   value to the executable that interprets it and the file extension its
   snippets need.
2. The **script fragment renderers** (:func:`cd_to`, :func:`prompt`,
   :func:`probe`, :func:`set_var`, :func:`unset_var`, …). One shell's syntax per
   arm, composed by the test into a script that a real interpreter runs.
3. The **fixture builders** (:func:`write_project`, :func:`write_lock`,
   :func:`clean_env`, …) — enough on-disk state for the reconciler to have an
   opinion, with no registry and no ``src.runner``.

The session model is the point. A test builds one script that runs in **one**
shell process and calls ``ocx self activate --reconcile`` between ``cd``s — the
same thing the emitted prompt hook does — so the ``__OCX_ENV_STATE`` carrier
propagates the way it does in a real session rather than being threaded by the
test. Nushell is the exception: it has no string ``eval`` (A-24), so its snippets
must go through :func:`eval_snippet`, one prompt per process.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import select
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

# Printed in place of a variable's value when the variable is unset. Distinct
# from the empty string on purpose: `Some("")` is a set-but-empty prior and must
# restore through the constant exporter, never through `unset` (A-05).
ABSENT = "__OCX_ABSENT__"

# Probe lines are `@@<label>@@<value>`. A marker is needed because an
# interactive-ish shell, a pty session or a `printf ... >&2` diagnostic can put
# arbitrary bytes on the same stream.
_MARK = "@@"

# The private state carrier (C-001). Mirrored here rather than imported so the
# module stays stdlib-only; `test_shell_reconcile.py` asserts it against the
# binary's own `ocx shell state` output, so a rename cannot go unnoticed.
CARRIER = "__OCX_ENV_STATE"

# A clean, minimal base PATH. The ocx bin dir and every project bin dir are
# deliberately absent, so "the reconciler put it there" is never vacuous.
BASE_PATH = os.pathsep.join(["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin", "/sbin", "/bin"])


@dataclass(frozen=True)
class Arm:
    """One shell arm: what ocx is asked to emit, and what interprets it."""

    shell: str
    """The ``--shell=`` value passed to ``ocx self activate``."""

    binary: str
    """The executable name looked up with :func:`shutil.which`."""

    extension: str
    """Snippet/script file extension. ``pwsh`` refuses to dot-source anything
    that is not ``.ps1``; nushell's ``source`` resolves at parse time and wants
    ``.nu``."""

    flags: tuple[str, ...] = ()
    """Flags inserted before the script path so no user rc file is read."""

    evaluates_strings: bool = True
    """Whether the arm can ``eval`` a *string* produced at run time. False for
    nushell (A-24), which therefore cannot host a multi-prompt session."""


ARMS: dict[str, Arm] = {
    # `sh` is the POSIX alias for `--shell=dash`; the *binary* is still `sh`,
    # which is what makes it a distinct arm rather than a duplicate of dash.
    "sh": Arm("sh", "sh", ".sh"),
    "dash": Arm("dash", "dash", ".sh"),
    "ash": Arm("ash", "ash", ".sh"),
    "bash": Arm("bash", "bash", ".sh", ("--norc",)),
    "zsh": Arm("zsh", "zsh", ".sh", ("--no-rcs",)),
    "fish": Arm("fish", "fish", ".fish", ("--no-config",)),
    "pwsh": Arm("pwsh", "pwsh", ".ps1", ("-NoProfile", "-File")),
    "nushell": Arm("nushell", "nu", ".nu", ("--no-config-file",), evaluates_strings=False),
    "elvish": Arm("elvish", "elvish", ".elv"),
}

# Every arm, in the order the plan's shell-zoo list names them.
ALL_SHELLS: tuple[str, ...] = ("sh", "dash", "ash", "bash", "zsh", "fish", "pwsh", "nushell", "elvish")

# The arms that can host a multi-prompt session in one process.
SESSION_SHELLS: tuple[str, ...] = tuple(name for name in ALL_SHELLS if ARMS[name].evaluates_strings)


# ---------------------------------------------------------------------------
# Binary + environment
# ---------------------------------------------------------------------------


def ocx_binary() -> Path | None:
    """Resolve the ocx binary under test, or ``None`` to skip the module.

    Same precedence as ``test_shell_activation.py``: an explicit override first
    (the shell-zoo container mounts one), then the prebuilt ``test/bin/ocx``.
    """
    for key in ("OCX_ACTIVATION_BINARY", "OCX_COMMAND"):
        value = os.environ.get(key)
        if value and Path(value).is_file():
            return Path(value)
    fallback = Path(__file__).resolve().parents[1] / "bin" / ("ocx.exe" if os.name == "nt" else "ocx")
    if fallback.is_file():
        return fallback
    # In the zoo the module sits at /work/shell_matrix.py beside /work/ocx.
    mounted = Path(__file__).resolve().parent / "ocx"
    return mounted if mounted.is_file() else None


def clean_env(home: Path, shell_abs: str, *, ocx_home: Path, **extra: str) -> dict[str, str]:
    """Build a clean child env: HOME, OCX_HOME and a minimal PATH, nothing else.

    No ambient ``OCX_*`` leaks in, and the shell's own directory is appended so
    an arm that re-execs a helper still resolves it.
    """
    path = BASE_PATH
    shell_dir = str(Path(shell_abs).parent)
    if shell_dir not in path.split(os.pathsep):
        path = path + os.pathsep + shell_dir
    env = {"HOME": str(home), "OCX_HOME": str(ocx_home), "PATH": path}
    env.update({key: value for key, value in extra.items() if value is not None})
    return env


def which_arm(shell: str) -> str | None:
    """Absolute path of the arm's interpreter, or ``None`` when it is absent."""
    return shutil.which(ARMS[shell].binary)


def missing_tool_is_fatal(tool: str) -> bool:
    """Whether an absent ``tool`` must fail rather than skip.

    A skip for a missing tool is indistinguishable from a pass: the assertions
    never run and nothing in the report says so. Off by default, because a host
    ``uv run pytest`` has few of the nine shells and a hard default would red
    the developer box rather than the behaviour under test. Set
    ``__OCX_TESTING_REQUIRE_LIVE_SHELLS`` where the tools DO exist — the
    shell-zoo images, a CI leg that installs them — as ``1``/``all`` for
    everything, or a comma-separated list of names (``fish,pwsh,script``).
    Python-side twin of the Rust seam of the same name in
    ``crates/ocx_lib/src/shell.rs``.

    ``tool`` is any binary the matrix needs, not only a shell arm — the pty
    driver is stdlib (:func:`pty_session`) precisely so no external tool can
    take a regression gate down with it, but anything that IS reached for goes
    through here, because "the tool left the image" and "the gate passed" must
    not read the same.
    """
    raw = os.environ.get("__OCX_TESTING_REQUIRE_LIVE_SHELLS", "").strip()
    if not raw:
        return False
    if raw == "1" or raw.lower() == "all":
        return True
    return tool in {name.strip() for name in raw.split(",") if name.strip()}


def missing_arm_is_fatal(shell: str) -> bool:
    """:func:`missing_tool_is_fatal` for a shell arm, by name or by binary.

    An arm answers to either spelling, so ``nushell`` and ``nu`` both name the
    same interpreter in the env var's list form.
    """
    return missing_tool_is_fatal(shell) or missing_tool_is_fatal(ARMS[shell].binary)


# ---------------------------------------------------------------------------
# On-disk fixtures — no registry, no `src.runner`
# ---------------------------------------------------------------------------

# A lock with no tools: its source set is empty, which is exactly the state
# clause 2's non-vacuity requirement exists to refuse.
EMPTY_LOCK_TOOLS = "tool = []\n"


def lock_tool(name: str, repository: str, *, digest: str | None = None) -> str:
    """One ``[[tool]]`` block, digest-pinned for three platforms.

    The digest is never fetched: consent reads the lock for its **source set**
    only (C-026), and every test that reaches composition uses an ``[env]``-only
    project. The three platform keys cover the CI matrix so a host-leaf lookup,
    if one ever happens, is not the thing that fails.
    """
    leaf = digest or ("sha256:" + "11" * 32)
    platforms = "\n".join(f'"{key}" = "{leaf}"' for key in ("linux/amd64", "linux/arm64", "darwin/arm64"))
    return f'[[tool]]\nname = "{name}"\ngroup = "default"\nrepository = "{repository}"\n\n[tool.platforms]\n{platforms}\n'


def write_lock(project_dir: Path, tools: str = EMPTY_LOCK_TOOLS, *, declaration_hash: str | None = None) -> Path:
    """Write an ``ocx.lock`` whose tool set is exactly ``tools``.

    ``declaration_hash`` only matters on the composition path (the staleness
    gate); consent and ``ocx shell state`` read the lock without it, which is
    what lets a source-set fixture be hand-written.
    """
    digest = declaration_hash or ("sha256:" + "00" * 32)
    lock = project_dir / "ocx.lock"
    lock.write_text(
        f"{tools}\n"
        "[metadata]\n"
        "lock_version = 3\n"
        "declaration_hash_version = 1\n"
        f'declaration_hash = "{digest}"\n'
        'generated_by = "ocx acceptance fixture"\n'
        'generated_at = "2026-01-01T00:00:00Z"\n',
        encoding="utf-8",
    )
    return lock


def declaration_hash_of(lock_path: Path) -> str:
    """The ``declaration_hash`` ``ocx lock`` recorded for the ``ocx.toml`` beside it.

    Read back, never recomputed. The composer refuses a lock whose hash does not
    match its declaration, and the refusal is *silent*: the reconcile degrades to
    emitting nothing while the fixture on disk still looks correct. So a test
    that needs to **edit** a generated lock — add tools the offline `ocx lock`
    could not resolve, say — carries the generated hash across verbatim instead
    of inventing one, and the declaration it was computed over stays untouched.
    """
    text = lock_path.read_text(encoding="utf-8")
    match = re.search(r'^declaration_hash\s*=\s*"([^"]+)"', text, re.MULTILINE)
    if match is None:
        raise AssertionError(f"no declaration_hash in '{lock_path}':\n{text}")
    return match.group(1)


def record_origin(ocx_home: Path, *, registry: str, digest: str, origin: str) -> Path:
    """Write the pull-origin marker clause 2's evidence is quantified over.

    ``project::consent::verified_sources`` corroborates a locked tool by asking
    the package store which **logical repository** it recorded a genuine fetch of
    that tool's digest under. The record is one plain file per repository under
    ``<store>/refs/origins/``, named by
    ``ReferenceManager::name_for_path`` — a 16-hex truncated SHA-256 — of its own
    content. This writes that pair.

    Why it can be written by hand at all: the marker is a *file*, not a
    signature, and the acceptance arena's whole point is to reach the consent
    predicate without a registry. Three derivations are mirrored here — the store
    layout (``<packages>/<registry>/<algorithm>/<hex[:2]>/<hex[2:32]>``), the
    marker name, and the origin string — and **every one of them fails closed**:
    a wrong path, a wrong name or a wrong content all end as "no recorded origin
    for this tool", which makes ``verified_sources`` return ``None`` and clause 2
    refuse. A caller must therefore *observe* the grant it expected rather than
    assume this landed — the binary is the only authority on whether it did.

    ``registry`` is used as its own directory name, so pass one that is already a
    slug (``ocx.sh``); the store slugifies, and a registry needing escapes would
    land the marker somewhere the binary does not look — fail-closed again, but
    for a reason no assertion would name.
    """
    algorithm, _, hex_digest = digest.partition(":")
    # Layout mirrors `PackageStore::path` + `cas_shard_path`: 2-hex shard prefix,
    # 30-hex suffix, the full digest recoverable only from the sibling `digest`
    # file (which nothing on the consent path reads).
    origins = (
        ocx_home / "packages" / registry / algorithm / hex_digest[:2] / hex_digest[2:32] / "refs" / "origins"
    )
    origins.mkdir(parents=True, exist_ok=True)
    marker = origins / hashlib.sha256(origin.encode("utf-8")).hexdigest()[:16]
    marker.write_text(origin, encoding="utf-8")
    return marker


def write_project(project_dir: Path, env_block: str, *, tools_block: str = "") -> Path:
    """Write an ``ocx.toml`` carrying ``env_block`` under ``[env]``."""
    project_dir.mkdir(parents=True, exist_ok=True)
    body = f"{tools_block}\n[env]\n{env_block}\n" if tools_block else f"[env]\n{env_block}\n"
    config = project_dir / "ocx.toml"
    config.write_text(body, encoding="utf-8")
    return config


def run_lock(ocx: Path, project_dir: Path, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    """Run ``ocx lock`` — one of the six commands that records a consent stamp.

    The acceptance-level way to reach "post-``add`` active": ``lock`` is on
    A-29's closed writer allowlist, so this both produces the lock the composer
    needs and stamps consent for the project directory.
    """
    return subprocess.run(
        [str(ocx), "--offline", "lock"],
        cwd=str(project_dir),
        capture_output=True,
        check=False,
        text=True,
        env=env,
    )


def stamp_dir(ocx_home: Path, project_key: str) -> Path:
    """The per-project consent-stamp root, ``state/projects/<key>/`` (C-022)."""
    return ocx_home / "state" / "projects" / project_key


def project_key(ocx: Path, project_dir: Path, env: dict[str, str]) -> str:
    """Ask the binary for the project key rather than re-deriving it.

    ``ReferenceManager::name_for_path`` is a truncated SHA-256 over the
    canonical directory; re-implementing it here would make the test agree with
    itself instead of with the binary.
    """
    return shell_state(ocx, project_dir, env)["project_key"]


def shell_state(ocx: Path, cwd: Path, env: dict[str, str]) -> dict:
    """``ocx shell state --format json``, parsed.

    The registry-free observation point for the whole consent set: the reason
    ladder in ``command/shell_state.rs`` renders every enumerated inertness
    reason (C-050), and the command writes nothing at all (A-29).
    """
    result = subprocess.run(
        [str(ocx), "--offline", "--format", "json", "shell", "state"],
        cwd=str(cwd),
        capture_output=True,
        check=False,
        text=True,
        env=env,
    )
    if result.returncode != 0:
        raise AssertionError(f"ocx shell state exited {result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}")
    return json.loads(result.stdout)


def reconcile(
    ocx: Path,
    shell: str,
    cwd: Path,
    env: dict[str, str],
    *,
    carrier: str | None = None,
    extra: tuple[str, ...] = (),
) -> subprocess.CompletedProcess[str]:
    """Run one ``ocx self activate --reconcile`` and return the raw process.

    The emitted stream is stdout; the hook path exits 0 in every state (C-051),
    so a caller asserting on the exit code is asserting on the contract, not on
    an incidental.
    """
    child = dict(env)
    if carrier is not None:
        child[CARRIER] = carrier
    return subprocess.run(
        [str(ocx), "--offline", "self", "activate", "--reconcile", f"--shell={shell}", *extra],
        cwd=str(cwd),
        capture_output=True,
        check=False,
        text=True,
        env=child,
    )


# ---------------------------------------------------------------------------
# Script fragment renderers — one shell's syntax per arm
# ---------------------------------------------------------------------------


def _single_quote_posix(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


def _single_quote_pwsh(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _double_quote_nu(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def quote(shell: str, value: str) -> str:
    """Quote ``value`` as a literal in ``shell``'s own syntax."""
    if shell == "pwsh":
        return _single_quote_pwsh(value)
    if shell == "nushell":
        return _double_quote_nu(value)
    # fish and elvish both take POSIX-style single quotes; fish escapes an inner
    # quote with a backslash rather than the `'\''` dance, and elvish doubles it.
    if shell == "fish":
        return "'" + value.replace("\\", "\\\\").replace("'", "\\'") + "'"
    if shell == "elvish":
        return "'" + value.replace("'", "''") + "'"
    return _single_quote_posix(value)


def header(shell: str, ocx: Path) -> str:
    """Bind ``__ocx_exe`` to the binary under test, once per script."""
    literal = quote(shell, str(ocx))
    if shell == "pwsh":
        return f"$__ocx_exe = {literal}"
    if shell == "fish":
        return f"set -g __ocx_exe {literal}"
    if shell == "elvish":
        return f"var __ocx_exe = {literal}"
    if shell == "nushell":
        return f"let __ocx_exe = {literal}"
    return f"__ocx_exe={literal}"


def cd_to(shell: str, path: Path | str) -> str:
    """Change directory — the event the whole feature reacts to."""
    literal = quote(shell, str(path))
    if shell == "pwsh":
        return f"Set-Location -LiteralPath {literal}"
    return f"cd {literal}"


def prompt(shell: str, *, extra: str = "") -> str:
    """One simulated prompt: run the reconciler and evaluate what it emits.

    This is the hook body's whole job, written by hand so a test can place it
    where it wants. ``extra`` appends flags to the ocx invocation.
    """
    args = f"--offline self activate --reconcile --shell={shell} {extra}".strip()
    if shell == "pwsh":
        return (
            f"$__ocx_out = (& $__ocx_exe {args} | Out-String)\n"
            "if ($__ocx_out.Trim()) { Invoke-Expression $__ocx_out }"
        )
    if shell == "fish":
        return (
            f"set -l __ocx_out ($__ocx_exe {args} | string collect)\n"
            'if test -n "$__ocx_out"; eval $__ocx_out; end'
        )
    if shell == "elvish":
        return (
            f"var __ocx_out = ((external $__ocx_exe) {args} | slurp)\n"
            "if (not-eq $__ocx_out '') { eval $__ocx_out }"
        )
    if shell == "nushell":
        raise AssertionError(
            "nushell has no string eval (A-24): drive it through eval_snippet, one prompt per process"
        )
    return f'eval "$("$__ocx_exe" {args})"'


def probe(shell: str, label: str, name: str) -> str:
    """Print ``@@<label>@@<value>``, or the :data:`ABSENT` sentinel when unset.

    Set-but-empty and unset must stay distinguishable (A-05), so this never
    collapses them.
    """
    if shell == "pwsh":
        return (
            f"if (Test-Path env:{name}) {{ Write-Output ('{_MARK}{label}{_MARK}' + $env:{name}) }} "
            f"else {{ Write-Output '{_MARK}{label}{_MARK}{ABSENT}' }}"
        )
    if shell == "fish":
        # A fish variable is a list; join it back to the OS PATH form so PATH and
        # a scalar read the same way.
        # `--` before the separator: a value like `-DPROJECT_A` is otherwise read
        # as an option by `string join`.
        return (
            f"if set -q {name}; printf '%s\\n' '{_MARK}{label}{_MARK}'(string join -- : ${name}); "
            f"else; printf '%s\\n' '{_MARK}{label}{_MARK}{ABSENT}'; end"
        )
    if shell == "elvish":
        return (
            f"if (has-env {name}) {{ echo '{_MARK}{label}{_MARK}'$E:{name} }} "
            f"else {{ echo '{_MARK}{label}{_MARK}{ABSENT}' }}"
        )
    if shell == "nushell":
        return (
            f"let __v = ($env.{name}? | default {_double_quote_nu(ABSENT)}); "
            f"print ({_double_quote_nu(_MARK + label + _MARK)} + "
            "(if ($__v | describe | str starts-with 'list') { $__v | str join (char esep) } else { $__v }))"
        )
    # `${NAME-…}` rather than `${NAME:-…}`: set-but-empty must survive `set -u`
    # and must not be reported as absent.
    return f"printf '%s\\n' \"{_MARK}{label}{_MARK}${{{name}-{ABSENT}}}\""


def set_var(shell: str, name: str, value: str) -> str:
    """Export ``name`` in the running shell — the "prior" a revert restores."""
    literal = quote(shell, value)
    if shell == "pwsh":
        return f"$env:{name} = {literal}"
    if shell == "fish":
        return f"set -gx {name} {literal}"
    if shell == "elvish":
        return f"set E:{name} = {literal}"
    if shell == "nushell":
        return f"$env.{name} = {literal}"
    return f"export {name}={literal}"


def unset_var(shell: str, name: str) -> str:
    """Remove ``name`` from the running shell's environment."""
    if shell == "pwsh":
        return f"Remove-Item env:{name} -ErrorAction SilentlyContinue"
    if shell == "fish":
        return f"set -e {name}"
    if shell == "elvish":
        return f"unset-env {name}"
    if shell == "nushell":
        return f"hide-env --ignore-errors {name}"
    return f"unset {name}"


def strict_mode(shell: str) -> str:
    """Turn on the arm's "unset variable is an error" mode, where it has one."""
    if shell in ("sh", "dash", "ash", "bash", "zsh"):
        return "set -u"
    # fish, pwsh, nushell and elvish have no `set -u` equivalent: referencing an
    # unset variable is not an error in any of them.
    return ""


# ---------------------------------------------------------------------------
# Running a script / snippet
# ---------------------------------------------------------------------------


def run_script(
    shell: str,
    shell_abs: str,
    body: str,
    *,
    cwd: Path,
    env: dict[str, str],
    script_dir: Path,
    name: str = "session",
    timeout: int = 180,
) -> subprocess.CompletedProcess[str]:
    """Write ``body`` to a script file and run it in ``shell``."""
    arm = ARMS[shell]
    script = script_dir / f"{name}{arm.extension}"
    script.write_text(body.rstrip() + "\n", encoding="utf-8")
    return subprocess.run(
        [shell_abs, *arm.flags, str(script)],
        cwd=str(cwd),
        capture_output=True,
        check=False,
        text=True,
        env=env,
        timeout=timeout,
    )


def eval_snippet(
    shell: str,
    shell_abs: str,
    snippet: str,
    body: str,
    *,
    cwd: Path,
    env: dict[str, str],
    script_dir: Path,
    name: str = "snippet",
    timeout: int = 180,
) -> subprocess.CompletedProcess[str]:
    """Evaluate a *pre-captured* ``snippet`` in ``shell``, then run ``body``.

    The one-shot counterpart to :func:`prompt`: needed for nushell, whose
    ``source`` resolves at parse time and so cannot consume a string produced
    during the same run, and useful anywhere a test wants to feed a synthetic
    or mutated stream.
    """
    arm = ARMS[shell]
    snippet_file = script_dir / f"{name}_snippet{arm.extension}"
    snippet_file.write_text(snippet if snippet.endswith("\n") else snippet + "\n", encoding="utf-8")
    literal = quote(shell, str(snippet_file))
    if shell == "pwsh":
        load = f". {literal}"
    elif shell == "fish":
        load = f"source {literal}"
    elif shell == "elvish":
        load = f"eval (slurp < {literal})"
    elif shell == "nushell":
        load = f"source {literal}"
    else:
        load = f". {literal}"
    return run_script(
        shell,
        shell_abs,
        f"{load}\n{body}",
        cwd=cwd,
        env=env,
        script_dir=script_dir,
        name=name,
        timeout=timeout,
    )


# CSI and OSC escape sequences, stripped before probe lines are scanned. A pty
# transcript interleaves the shell's own rendering with the command output, and
# an OSC title update can land in the middle of a probe line.
_ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\r")
_PROBE = re.compile(rf"{_MARK}([^@]+){_MARK}(.*)$")


def probes(stdout: str) -> dict[str, str]:
    """Parse every ``@@label@@value`` probe out of a shell's output.

    Scans **anywhere in a line, last occurrence wins**, because an interactive
    shell echoes the probe command before it runs it — so the line carries the
    literal `printf "…@@const@@${WP14_CONST-…}"` first and the produced value
    second. Taking the first match would read back the fixture's own source text
    and pass for the wrong reason.
    """
    found: dict[str, str] = {}
    for raw in _ANSI.sub("", stdout).splitlines():
        matches = list(_PROBE.finditer(raw.strip()))
        if not matches:
            continue
        last = matches[-1]
        found[last.group(1)] = last.group(2)
    return found


def path_segments(value: str) -> list[str]:
    """Split a PATH-shaped value into its non-empty segments."""
    return [segment for segment in value.split(os.pathsep) if segment]


# ---------------------------------------------------------------------------
# Driving a shell on a real pty
# ---------------------------------------------------------------------------


def line_editor_is_reading(master: int) -> bool:
    """Whether a line editor currently owns the pty — the keystroke handshake.

    A pty starts in **canonical** mode with ``ICRNL`` on, and every line editor
    the matrix drives (readline, ZLE, PSReadLine) clears ``ICANON`` for the
    duration of one ``ReadLine`` call and restores it while the command runs.
    So "not canonical" is a direct observation of "the shell is at a prompt,
    reading keystrokes" — which silence cannot supply: a descheduled process is
    silent while it is still busy, and that is the state a loaded ``-n auto``
    run produces.

    Feeding into the canonical state is not merely early, it corrupts: the line
    discipline translates the ``\\r`` to ``\\n`` (``ICRNL``), PSReadLine treats
    ``\\n`` as ordinary input rather than accept-line, and the next fed line is
    appended to the unaccepted one — both then run as one mangled command.
    ``tcgetattr`` on the **master** fd reports the pair's line discipline, so
    the state is readable without touching the child.
    """
    import termios

    try:
        return not termios.tcgetattr(master)[3] & termios.ICANON
    except termios.error:
        # The child is gone; there is nothing left to feed.
        return False


def pty_session(
    command: list[str],
    lines: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout: int = 120,
) -> str:
    """Run ``command`` on a real pty and return the whole transcript.

    ``lines`` are fed as keystrokes, followed by ``exit``; an **empty** list
    feeds nothing at all and simply reads until the process exits, which is
    what a row wants when the shell's own startup files carry the probe (an
    interactive login shell only needs the tty, not a typist).

    stdlib ``pty`` rather than ``script(1)``: a missing ``script`` binary would
    turn every pty row into a skip that proves nothing, neither shell-zoo image
    ships one (Debian moved it to ``bsdextrautils``), and the two ``script``
    flavors disagree on both their argument shape and their stdin forwarding —
    so the ``script`` path also cost a platform-shaped skip on macOS.

    Four details are load-bearing, all learned the hard way and all silent when
    wrong:

    * **The pty must have a window size, and a wide one.** PSReadLine throws
      inside ``ForceRender`` on a 0x0 terminal, and mangles a line that wraps.
    * **Something must answer DSR.** A REPL line editor emits ``ESC [ 6 n`` and
      waits for the cursor-position reply before it processes input; with no
      responder the session hangs and reads as "the hook never fired".
    * **Enter is CR.** In the raw mode a line editor sets, an LF is ordinary
      input: pwsh accumulated every fed line into one and evaluated none.
    * **Silence is not consumption.** A line is fed only once the previous one
      produced output, that output went quiet, *and*
      :func:`line_editor_is_reading` says a line editor owns the pty. The quiet
      count alone is a wall-clock heuristic: under parallel load the shell can
      be starved for longer than it, and the feed then lands in the canonical
      window between two prompts, where the ``\\r`` is mistranslated and two
      lines merge into one.

    A session that never ends raises :class:`TimeoutError`; it does **not**
    return the partial transcript. Returning it made this whole harness
    vacuous: every caller asserts that some marker appears, and a shell that
    prints its markers and then hangs satisfies that assertion exactly as well
    as one that ran to completion. The two states have to be distinguishable,
    and the only place that can tell them apart is here.
    """
    import fcntl
    import pty
    import struct
    import termios

    master, slave = pty.openpty()
    # A wide window, deliberately: PSReadLine mangles a wrapped line on a pty,
    # and every path in these fixtures is long.
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 100, 400, 0, 0))
    process = subprocess.Popen(
        command,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        cwd=str(cwd),
        env=env,
        close_fds=True,
        start_new_session=True,
    )
    os.close(slave)
    collected = bytearray()
    pending = [*lines, "exit"] if lines else []
    quiet_ticks = 0
    # Whether the shell has produced anything since the last line was fed. The
    # first line has no predecessor to wait on.
    answered = True
    deadline = time.monotonic() + timeout
    exited = False
    try:
        while time.monotonic() < deadline:
            readable, _, _ = select.select([master], [], [], 0.25)
            if readable:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                collected.extend(chunk)
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
                # A bare cursor-position query is the line editor's heartbeat,
                # not output. Counting it as activity keeps `quiet_ticks` at
                # zero forever, so the next line is never fed and the session
                # times out looking exactly like "the hook never fired".
                if chunk.replace(b"\x1b[6n", b"").strip():
                    quiet_ticks = 0
                    answered = True
                continue
            if process.poll() is not None:
                break
            quiet_ticks += 1
            if not pending or quiet_ticks < 3 or not answered:
                continue
            if not line_editor_is_reading(master):
                # Quiet but canonical: the previous command has not handed the
                # pty back to a line editor yet. Feeding here is what merges
                # two lines into one.
                continue
            # CR, never LF: a line editor puts the pty in raw mode, where the
            # Enter key is carriage return. PSReadLine buffers an LF as
            # ordinary input and never evaluates the line.
            os.write(master, (pending.pop(0) + "\r").encode())
            quiet_ticks = 0
            answered = False
    finally:
        if process.poll() is not None:
            exited = True
        else:
            # Asked BEFORE the master is closed, and that order is the whole
            # test: closing it hangs up the terminal, and the SIGHUP makes even
            # a wedged shell exit — so a check on the far side of the hangup
            # reports "ended normally" for every session, hung or not.
            # The pty can also reach EOF a hair before the child is reaped, so
            # a short wait here, not a bare poll.
            try:
                process.wait(timeout=5)
                exited = True
            except subprocess.TimeoutExpired:
                pass
        try:
            os.close(master)
        except OSError:
            pass
        if process.poll() is None:
            process.kill()
            process.wait(timeout=15)
    if not exited:
        raise TimeoutError(
            f"pty session did not end within {timeout}s: {command!r} was still running and was "
            f"killed. {len(pending)} of {len(lines) + 1 if lines else 0} line(s) were never fed "
            f"({pending!r}). The transcript below is PARTIAL — asserting over it would pass a "
            f"session that printed its markers and then hung:\n{collected.decode('utf-8', 'replace')}"
        )
    return collected.decode("utf-8", "replace")
