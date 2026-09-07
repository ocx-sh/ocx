# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the session-PATH arm of ``ocx self setup`` (WP-12c).

``ocx self setup`` registers two directories at **session** level, in order:
the ocx install ``bin`` directory and ``$OCX_HOME/toolchain/bin`` (S-002,
C-036). Each platform owns one store — ``HKCU\\Environment\\Path`` on Windows,
``~/.config/environment.d/ocx.conf`` on Linux, a ``RunAtLoad`` LaunchAgent on
macOS (C-038 / C-039 / C-040).

The load-bearing case here is the **Linux runtime proof**. The
``environment.d`` prepend has open upstream reports of not landing for ``PATH``
specifically (systemd/systemd#40430), and the two halves of the doubt are:

1. does the *session manager* pick the file up — needs a session bus and
   ``loginctl enable-linger``, and is **out of scope**; and
2. does systemd's own reader expand ``PATH=<dirs>:$PATH`` into a value where
   our directories lead the inherited PATH — which is exactly what
   ``30-systemd-environment-d-generator`` decides, on stdout, with no bus and
   no lingering.

Half 2 is the one under doubt and the one proved here: the real generator is
run over the **writer's real output**, with a controlled ``XDG_CONFIG_HOME`` /
``HOME``, and its stdout is parsed. The value the writer's conf must lead is
*measured* by running the generator without that conf — the system-level
``environment.d`` files are read whatever the tests do, and on a GitHub runner
one of them replaces PATH outright.

A skip for a missing generator is indistinguishable from a pass, so the probe
names the paths **it observed missing** and
``__OCX_TESTING_REQUIRE_ENVIRONMENT_D`` turns the absence into a failure — the
same live-gate shape ``__OCX_TESTING_REQUIRE_LIVE_SHELLS`` already uses in
``src/shell_matrix.py``. The Linux acceptance leg of ``verify-deep.yml``
exports it.

``deregister_session_path`` has **no CLI caller** on any platform, so S-014's
removal half is not reachable from this harness at all; its coverage is the
Rust unit tests in ``crates/ocx_lib/src/setup/session_path/{linux,macos,
windows}.rs``. What is reachable — and asserted here — is S-014's survivor
premise on the register path: a foreign ``environment.d`` prepend planted
before ``ocx self setup`` is still on the generated PATH afterwards.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Sequence
from pathlib import Path

import pytest

from src.runner import OcxRunner

# The install-layout path the bootstrap candidate lives at, relative to
# OCX_HOME. Same constant as `test_self_setup.py`; seeding it lets the offline
# bootstrap resolve `already_present` so no registry is involved.
_CANDIDATE_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"

# The two directories `setup::session_path_directories` registers, in the order
# C-060 fixes them: the installed ocx first, the composed toolchain second.
_INSTALL_BIN_REL = _CANDIDATE_REL.parent
_TOOLCHAIN_BIN_REL = Path("toolchain") / "bin"

# systemd ships the user-environment generators under `/usr/lib` on a merged-
# /usr distribution and under `/lib` on the split layout; Fedora and some
# derivatives also carry a `/usr/libexec` spelling. All three are probed so a
# skip can name every path it actually looked at.
_GENERATOR_CANDIDATES = (
    "/usr/lib/systemd/user-environment-generators/30-systemd-environment-d-generator",
    "/lib/systemd/user-environment-generators/30-systemd-environment-d-generator",
    "/usr/libexec/systemd/user-environment-generators/30-systemd-environment-d-generator",
)

# The PATH handed to the generator *process*. Two segments, both certain to
# exist on any host that has systemd at all, and neither of them a directory
# ocx writes — so no assertion here rides on the runner's own PATH.
#
# It is only the starting value: the generator also reads the system-level
# `environment.d` directories, and `/usr/lib/environment.d/99-environment.conf`
# is a symlink to `/etc/environment` on Debian, Ubuntu and Fedora alike. Where
# that file assigns PATH outright (a GitHub runner ships a long one), it
# *replaces* this value before any file the tests write is read. So what the
# ocx conf prepends to is measured by `_baseline_path`, never assumed to be
# this constant.
_INHERITED_PATH = "/usr/bin:/bin"

# The character each platform's session-PATH format cannot encode, and the
# `SessionPathFormat` Display phrase the refusal names (C-037, S-015).
_UNENCODABLE = {
    "linux": ("$", "an environment.d configuration file"),
    "darwin": ("'", "a launchd LaunchAgent property list"),
    "win32": ("%", "the Windows user PATH registry value (REG_EXPAND_SZ)"),
}


# ---------------------------------------------------------------------------
# Generator probe — anti-skip
# ---------------------------------------------------------------------------


def _absence_is_fatal() -> bool:
    """Whether a missing ``environment.d`` generator must fail rather than skip.

    Fails **closed**: any value other than empty or ``0`` arms the gate, so a
    typo in the workflow (``=on``, ``=yes``) still fails the run rather than
    silently disarming the one check the leg exists to carry. Off by default —
    a developer host may have no systemd at all, and a hard default would red
    the box rather than the behaviour under test.

    Python-side twin of ``__OCX_TESTING_REQUIRE_LIVE_SHELLS``
    (``src/shell_matrix.py``); the ``__OCX_TESTING_`` prefix keeps it off the
    product surface ``reference/environment.md`` enumerates.
    """
    raw = os.environ.get("__OCX_TESTING_REQUIRE_ENVIRONMENT_D", "").strip()
    return bool(raw) and raw != "0"


def _environment_d_generator() -> str:
    """systemd's ``environment.d`` generator, or skip naming what was missing.

    The skip message enumerates the paths **it observed**, never a generic
    "systemd not available": a reason nobody measured outlives the condition it
    describes, and the next reader cannot tell a moved binary from an absent
    one.
    """
    missing: list[str] = []
    for candidate in _GENERATOR_CANDIDATES:
        if os.access(candidate, os.X_OK):
            return candidate
        missing.append(candidate)
    probed = ", ".join(missing)
    assert not _absence_is_fatal(), (
        "systemd's environment.d generator is not executable at any probed path, so this "
        f"test asserted nothing — probed and found missing: {probed}; and "
        "__OCX_TESTING_REQUIRE_ENVIRONMENT_D names it as one that must be live here"
    )
    pytest.skip(f"systemd's environment.d generator is not executable at any probed path: {probed}")


def _generator_path(config_home: Path, home: Path) -> str | None:
    """systemd's generated ``PATH``, or ``None`` when no file assigns one.

    The child environment is exactly three variables, so the value it prints is
    a function of the conf files the generator reads — the ones under
    ``config_home`` plus the system-level ones — and :data:`_INHERITED_PATH`.
    """
    generator = _environment_d_generator()
    result = subprocess.run(
        [generator],
        capture_output=True,
        text=True,
        check=False,
        env={"HOME": str(home), "XDG_CONFIG_HOME": str(config_home), "PATH": _INHERITED_PATH},
    )
    assert result.returncode == 0, (
        f"{generator} exited {result.returncode}\nstdout: {result.stdout!r}\nstderr: {result.stderr!r}"
    )
    values = [line.removeprefix("PATH=") for line in result.stdout.splitlines() if line.startswith("PATH=")]
    assert len(values) <= 1, (
        f"{generator} must emit at most one PATH assignment; got {len(values)} in {result.stdout!r}"
    )
    return values[0] if values else None


def _generated_path(config_home: Path, home: Path) -> str:
    """:func:`_generator_path` where a conf under test is known to set PATH."""
    value = _generator_path(config_home, home)
    assert value is not None, (
        f"the generator emitted no PATH assignment for {config_home}, whose conf files set PATH"
    )
    return value


def _baseline_path(config_home: Path, home: Path, exclude: str) -> str:
    """The PATH the generator computes **without** the conf named ``exclude``.

    Every other conf under ``config_home`` is copied into a scratch config home
    and the generator is run there, so the value is exactly what the ocx conf
    prepends to: the system-level files plus whatever else the test planted.

    Measured rather than assumed. The system-level files are read whatever the
    tests do, and where one of them assigns PATH outright it replaces
    :data:`_INHERITED_PATH` before any of the tests' own confs are seen — which
    is why an assumed constant here reds on a GitHub runner for a reason that
    has nothing to do with ocx.
    """
    source = config_home / "environment.d"
    with tempfile.TemporaryDirectory() as scratch:
        target = Path(scratch) / "environment.d"
        target.mkdir()
        for conf in sorted(source.iterdir()) if source.is_dir() else ():
            if conf.name != exclude:
                shutil.copy2(conf, target / conf.name)
        return _generator_path(Path(scratch), home) or _INHERITED_PATH


def _assert_leads(path_value: str, directories: Sequence[str], baseline: str) -> None:
    """Assert ``directories`` appear in order, ahead of every ``baseline`` segment.

    ``baseline`` is what the generator produced without the conf under test —
    :func:`_baseline_path`, or an earlier generator run where the test planted
    another conf first.

    Both halves are load-bearing and both have their own reachable red, pinned
    by the two positive controls below: an append leaves the directories
    *behind* the baseline, and a line that never expands ``$PATH`` makes the
    baseline segments vanish entirely. A check that only looked for the
    directories would pass on both.
    """
    segments = path_value.split(":")
    inherited = baseline.split(":")

    for segment in inherited:
        assert segment in segments, (
            f"the inherited PATH segment {segment!r} is gone from the generated value {path_value!r} — "
            "the conf replaced PATH instead of prepending to it"
        )
    for directory in directories:
        assert directory in segments, f"{directory!r} is not on the generated PATH {path_value!r}"

    positions = [segments.index(directory) for directory in directories]
    assert positions == sorted(positions), (
        f"the ocx directories are out of order on the generated PATH {path_value!r}: "
        f"expected {list(directories)} in that order"
    )
    first_inherited = min(segments.index(segment) for segment in inherited)
    assert max(positions) < first_inherited, (
        f"the ocx directories do not lead the inherited PATH in {path_value!r} — "
        f"they land at {positions}, the inherited PATH starts at {first_inherited}"
    )


# ---------------------------------------------------------------------------
# `ocx self setup` helpers
# ---------------------------------------------------------------------------


def _seed_candidate(ocx: OcxRunner) -> None:
    """Place a real ocx binary as the install candidate under ``OCX_HOME``.

    With the candidate present the offline bootstrap resolves
    ``already_present``, so these tests exercise the session-PATH arm alone and
    need no registry round-trip.
    """
    candidate = Path(ocx.env["OCX_HOME"]) / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ocx.binary, candidate)
    candidate.chmod(0o755)


def _setup(
    ocx: OcxRunner,
    home: Path,
    *extra_args: str,
    ocx_home: Path | None = None,
    fmt_json: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx --offline self setup`` with ``$HOME`` pinned to ``home``.

    ``$HOME`` is always overridden: the profile writer and the macOS
    LaunchAgent store both resolve from it, and a test that left it at the
    developer's real home would edit the machine it runs on.
    """
    home.mkdir(parents=True, exist_ok=True)
    command = [str(ocx.binary)]
    if fmt_json:
        command += ["--format", "json"]
    command += ["--offline", "self", "setup", *extra_args]
    env = {**ocx.env, "HOME": str(home)}
    if ocx_home is not None:
        env["OCX_HOME"] = str(ocx_home)
    return subprocess.run(command, capture_output=True, text=True, env=env, check=False)


def _session_path_entries(result: subprocess.CompletedProcess[str]) -> list[dict[str, str]]:
    """The ``session_path`` rows of a ``self setup`` JSON payload."""
    assert result.returncode == 0, f"self setup failed (rc={result.returncode})\nstderr: {result.stderr}"
    payload = json.loads(result.stdout)
    entries = payload["session_path"]
    assert entries, "session_path is empty — this host reported no session-PATH store at all"
    return entries


def _store(result: subprocess.CompletedProcess[str]) -> Path:
    """The single session-PATH store location ``self setup`` reported."""
    entries = _session_path_entries(result)
    assert len(entries) == 1, f"expected exactly one session-PATH store on this host, got {entries}"
    return Path(entries[0]["location"])


def _ocx_directories(ocx_home: Path) -> list[str]:
    """The two directories C-060 registers, in order."""
    return [str(ocx_home / _INSTALL_BIN_REL), str(ocx_home / _TOOLCHAIN_BIN_REL)]


linux_only = pytest.mark.skipif(
    sys.platform != "linux",
    reason=f"the environment.d store is the Linux session-PATH writer; this host is {sys.platform}",
)


# ---------------------------------------------------------------------------
# S-002 / C-036 / C-039 — the writer
# ---------------------------------------------------------------------------


@linux_only
def test_setup_registers_both_directories_in_the_environment_d_store(ocx: OcxRunner, tmp_path: Path) -> None:
    """S-002 / C-039: one store, reported ``written``, carrying the prepend line.

    The byte assertion is the half that survives a host with no systemd — the
    generator proof below is the semantic one, and it skips where the generator
    is absent.
    """
    _seed_candidate(ocx)
    result = _setup(ocx, tmp_path / "home")

    entries = _session_path_entries(result)
    assert entries[0]["outcome"] == "written", entries
    store = Path(entries[0]["location"])
    assert store.name == "ocx.conf", store
    assert store.parent.name == "environment.d", store

    install_bin, toolchain_bin = _ocx_directories(Path(ocx.env["OCX_HOME"]))
    assert store.read_text() == f"PATH={install_bin}:{toolchain_bin}:$PATH\n"


@linux_only
def test_a_second_setup_leaves_the_environment_d_store_unchanged(ocx: OcxRunner, tmp_path: Path) -> None:
    """C-036: re-running is ``unchanged`` and does not rewrite the store."""
    _seed_candidate(ocx)
    home = tmp_path / "home"
    store = _store(_setup(ocx, home))
    first = store.stat().st_mtime_ns

    entries = _session_path_entries(_setup(ocx, home))
    assert entries[0]["outcome"] == "unchanged", entries
    assert store.stat().st_mtime_ns == first, "an unchanged store must not be rewritten"


@linux_only
def test_no_modify_path_suppresses_the_session_path_arm(ocx: OcxRunner, tmp_path: Path) -> None:
    """C-043: the opt-out reports the store it did *not* touch, and writes none."""
    _seed_candidate(ocx)
    result = _setup(ocx, tmp_path / "home", "--no-modify-path")

    entries = _session_path_entries(result)
    assert entries[0]["outcome"] == "skipped_opt_out", entries
    # The store is still *named*: the writers never ran, so C-043 has the
    # caller enumerate them, and a run summary that reported a blank location
    # would leave the user with nothing to check by hand.
    store = Path(entries[0]["location"])
    assert store.name == "ocx.conf", store
    assert not store.exists(), "the opt-out wrote the store anyway"


# ---------------------------------------------------------------------------
# C-036 / R-W41 — the two advisory arms
# ---------------------------------------------------------------------------


@linux_only
def test_an_unwritable_store_warns_and_still_exits_zero(ocx: OcxRunner, tmp_path: Path) -> None:
    """C-036: a write failure is an outcome, not an error — warn, exit 0.

    ``environment.d`` is planted as a regular *file*, so the writer's
    ``create_dir_all`` fails on a path that cannot become a directory. That
    beats a permission bit as a fixture: it fails for root too, so the case
    does not quietly vanish in a container that runs as uid 0.
    """
    _seed_candidate(ocx)
    blocker = Path(ocx.env["XDG_CONFIG_HOME"]) / "environment.d"
    blocker.parent.mkdir(parents=True, exist_ok=True)
    blocker.write_text("not a directory\n")

    result = _setup(ocx, tmp_path / "home")

    entries = _session_path_entries(result)  # asserts exit 0 — the contract's own half
    assert entries[0]["outcome"] == "failed", entries
    assert "could not register the ocx directories on the session PATH" in result.stderr, result.stderr
    assert entries[0]["location"] in result.stderr, (
        f"the warning must name the store it could not write; stderr={result.stderr!r}"
    )
    assert "ocx self setup" in result.stderr, (
        f"the warning must name the re-run that retries; stderr={result.stderr!r}"
    )


@linux_only
def test_dry_run_previews_each_directory_and_stays_silent_once_current(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The ``--dry-run`` preview, and its ``== Written`` filter.

    A dry run reports the store as ``written`` while writing nothing, so
    without the preview the summary reads as a completed write. The filter is
    the other half: once the store is current the predicted outcome is
    ``unchanged`` and the preview must go quiet, or every later dry run
    announces a registration that would not happen.
    """
    _seed_candidate(ocx)
    home = tmp_path / "home"

    preview = _setup(ocx, home, "--dry-run")
    entries = _session_path_entries(preview)
    assert entries[0]["outcome"] == "written", entries
    store = Path(entries[0]["location"])
    assert not store.exists(), "a dry run must not write the store"

    # One line per directory, each naming the directory and the store.
    for directory in _ocx_directories(Path(ocx.env["OCX_HOME"])):
        assert f'would register "{directory}" in "{store}"' in preview.stderr, (
            f"the dry-run preview must name {directory!r}; stderr={preview.stderr!r}"
        )

    # Now make the store current, and predict again.
    assert _session_path_entries(_setup(ocx, home))[0]["outcome"] == "written"
    second = _setup(ocx, home, "--dry-run")
    assert _session_path_entries(second)[0]["outcome"] == "unchanged", second.stdout
    assert "would register" not in second.stderr, (
        f"a store predicted `unchanged` must produce no preview line; stderr={second.stderr!r}"
    )


# ---------------------------------------------------------------------------
# The environment.d runtime proof
# ---------------------------------------------------------------------------


@linux_only
def test_the_generator_puts_the_ocx_directories_ahead_of_the_inherited_path(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """systemd's own generator expands the writer's real conf as a prepend.

    The one runtime half of C-039 that does not need a session bus: the file
    ``ocx self setup`` just wrote is handed to
    ``30-systemd-environment-d-generator`` with a known inherited PATH, and the
    value it prints must carry both ocx directories, in order, ahead of every
    segment the generator produces without our conf. This is what
    systemd/systemd#40430 casts doubt on.
    """
    _seed_candidate(ocx)
    home = tmp_path / "home"
    store = _store(_setup(ocx, home))

    # The generator's search root is the store's own grandparent, so the
    # location the command reported is what is exercised — not a path this
    # test rebuilt from the same rule the writer used.
    config_home = store.parent.parent
    _assert_leads(
        _generated_path(config_home, home),
        _ocx_directories(Path(ocx.env["OCX_HOME"])),
        _baseline_path(config_home, home, store.name),
    )


@linux_only
def test_a_foreign_environment_d_prepend_survives_registration(ocx: OcxRunner, tmp_path: Path) -> None:
    """S-014's survivor premise, on the register path.

    A foreign ``environment.d`` file placed before ``ocx self setup`` still
    contributes its segment to the generated PATH afterwards: the writer owns
    exactly one file and adds to the value rather than replacing it.

    The other conf here is the reason the property is stated against a
    *measured* baseline. Debian, Ubuntu and Fedora all ship
    ``/usr/lib/environment.d/99-environment.conf`` as a symlink to
    ``/etc/environment``, which on a GitHub runner assigns PATH outright — so
    on that host every conf sorting before it contributes nothing, the foreign
    one included. Seeding a stand-in makes the runner's condition reproducible
    on any host, and fixes the foreign conf's name at the one position from
    which it can reach ``ocx.conf`` at all.

    (S-014's *removal* half has no CLI caller on any platform — see the module
    docstring — so it is covered by the Rust unit tests, not here.)
    """
    _seed_candidate(ocx)
    home = tmp_path / "home"
    config_home = Path(ocx.env["XDG_CONFIG_HOME"])
    environment_d = config_home / "environment.d"
    environment_d.mkdir(parents=True, exist_ok=True)
    (environment_d / "99-environment.conf").write_text('PATH="/snap/bin:/usr/bin:/bin:/snap/bin"\n')

    foreign_dir = "/opt/foreign/bin"
    foreign = environment_d / "foreign.conf"
    foreign.write_text(f"PATH={foreign_dir}:$PATH\n")

    store = _store(_setup(ocx, home))
    assert store.parent.parent == config_home, (
        f"the store {store} did not land under the configured XDG config home {config_home}"
    )

    generated = _generated_path(config_home, home)
    baseline = _baseline_path(config_home, home, store.name)
    assert foreign.read_text() == f"PATH={foreign_dir}:$PATH\n", "the foreign conf was rewritten"

    # Not decoration: a `10-` prefix on the foreign conf sorts it ahead of the
    # absolute assignment above, and the survivor claim would then be measuring
    # that file rather than the writer.
    assert foreign_dir in baseline.split(":"), (
        f"the foreign segment {foreign_dir!r} does not reach the generated PATH {baseline!r} even "
        f"without {store.name} — a conf sorting between the two replaced PATH outright"
    )
    assert foreign_dir in generated.split(":"), (
        f"the foreign segment {foreign_dir!r} is gone from the generated PATH {generated!r}"
    )
    _assert_leads(generated, _ocx_directories(Path(ocx.env["OCX_HOME"])), baseline)


# ---------------------------------------------------------------------------
# Positive controls — the two reds the ordering proof must have
# ---------------------------------------------------------------------------


def _generated_path_for(tmp_path: Path, line: str) -> tuple[str, str]:
    """The generator's PATH over a hand-written ``ocx.conf``, and its baseline."""
    config_home = tmp_path / "cfg"
    home = tmp_path / "home"
    home.mkdir(parents=True, exist_ok=True)
    conf = config_home / "environment.d" / "ocx.conf"
    conf.parent.mkdir(parents=True, exist_ok=True)
    conf.write_text(line)
    return _generated_path(config_home, home), _baseline_path(config_home, home, conf.name)


@linux_only
def test_the_ordering_proof_reds_on_an_append_instead_of_a_prepend(tmp_path: Path) -> None:
    """Control 1: ``PATH=$PATH:<dirs>`` must fail :func:`_assert_leads`.

    Without this the ordering assertion could be satisfied by mere presence,
    and a writer that appended would still report green.
    """
    directories = ["/opt/ocx/bin", "/opt/ocx/toolchain/bin"]
    generated, baseline = _generated_path_for(tmp_path, f"PATH=$PATH:{':'.join(directories)}\n")

    # The inherited PATH survived — this mutation is about order alone.
    assert generated.split(":")[: len(baseline.split(":"))] == baseline.split(":")
    with pytest.raises(AssertionError, match="do not lead the inherited PATH"):
        _assert_leads(generated, directories, baseline)


@linux_only
def test_the_ordering_proof_reds_when_the_line_does_not_expand_the_inherited_path(tmp_path: Path) -> None:
    """Control 2: ``PATH=<dirs>`` must fail :func:`_assert_leads`.

    A conf that assigns instead of prepending destroys the user's PATH. The
    directories still lead — they are all there is — so only the
    inherited-segment half of the assertion catches it.
    """
    directories = ["/opt/ocx/bin", "/opt/ocx/toolchain/bin"]
    generated, baseline = _generated_path_for(tmp_path, f"PATH={':'.join(directories)}\n")

    assert generated == ":".join(directories), generated
    with pytest.raises(AssertionError, match="is gone from the generated value"):
        _assert_leads(generated, directories, baseline)


# ---------------------------------------------------------------------------
# S-015 / C-037 — the pre-write refusal
# ---------------------------------------------------------------------------


def test_an_unencodable_ocx_home_is_refused_before_the_store_is_written(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """S-015 / C-037: exit 78, naming the directory and the format, nothing written.

    The offending character is the one *this host's* format cannot encode:
    ``$`` for ``environment.d``, ``'`` for the LaunchAgent plist, ``%`` for
    ``REG_EXPAND_SZ``.

    "Nothing written" is asserted over the **whole machine**, not just the
    session-PATH store: the documented promise is that a refused run leaves the
    machine byte-identical, and the store's own absence is compatible with the
    refusal firing late, after four other phases have already written. The
    ``env.sh`` assertion at the end is what separates the two.
    """
    if sys.platform not in _UNENCODABLE:
        pytest.skip(f"no session-PATH format is defined for {sys.platform}")
    character, format_phrase = _UNENCODABLE[sys.platform]

    hostile_home = tmp_path / f"a{character}b" / ".ocx"
    hostile_home.mkdir(parents=True)
    candidate = hostile_home / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ocx.binary, candidate)
    candidate.chmod(0o755)

    home = tmp_path / "home"
    result = _setup(ocx, home, ocx_home=hostile_home, fmt_json=False)

    assert result.returncode == 78, (
        f"expected exit 78 (ConfigError), got {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert str(hostile_home / _INSTALL_BIN_REL) in result.stderr, result.stderr
    assert format_phrase in result.stderr, result.stderr

    # Refused *before* writing: the store this host owns must not exist.
    if sys.platform == "linux":
        assert not (Path(ocx.env["XDG_CONFIG_HOME"]) / "environment.d" / "ocx.conf").exists()
    elif sys.platform == "darwin":
        assert not (home / "Library" / "LaunchAgents" / "sh.ocx.path.plist").exists()

    # ...and neither must anything an earlier phase writes. The store being
    # absent only proves the *session-PATH* arm did not run; the refusal used
    # to fire as phase 3.5, after the bootstrap, the managed-config fetch, the
    # `env.*` shims and the profile blocks had all already landed. `env.sh` is
    # the crispest witness: `setup::shims::write_shims` creates it in
    # `$OCX_HOME` unconditionally on any run that reaches phase 2, and no other
    # phase writes it.
    assert not (hostile_home / "env.sh").exists(), (
        "the refusal fired after the env shims were written, so a refused run did not leave the "
        "machine as it found it"
    )
