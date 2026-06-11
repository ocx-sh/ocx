# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx self setup`` (plan_self_setup.md Phase C.3.2).

``ocx self setup`` completes a bare-binary install. It runs three things in a
hard order (bootstrap FIRST, then shims, then profiles):

  1. **Bootstrap** - install the latest published ``ocx.sh/ocx/cli`` into the
     content store so the env shims have a ``current`` to point at. On a
     machine that already has an install, this is a no-op (``already_present``).
  2. **Shims** - write the five per-shell ``$OCX_HOME/env.*`` loader files
     (sh/fish/ps1/nu/elv), diff-gated (unchanged bytes are left untouched).
  3. **Profiles** - splice a versioned, content-hashed fenced block
     (``# >>> ocx v1 <hash8> >>>`` ... ``# <<< ocx <<<``) into the detected
     shell profiles, unless ``--no-modify-path``.

Re-running is safe: an unchanged setup is a no-op; a stale ocx-authored fence
is rewritten (format-upgrade); a fence the user edited is reported dirty and
left untouched (exit 82) unless ``--force`` is passed; a legacy
``# BEGIN ocx`` block is migrated to the v1 fence.

Test isolation: each test seeds a pre-placed candidate binary under the
isolated ``OCX_HOME`` so the offline bootstrap resolves ``already_present`` and
no registry is required for the non-bootstrap scenarios (shim/fence/dirty/
dry-run logic). The single success-path bootstrap test (which DOES need the
registry:2 fixture) follows the ``__OCX_SELF_IMAGE`` seam pattern from
``test_self_update.py``.

POSIX-only at module scope: the fence + shim behavior exercised here is
sh-flavored. Windows profile handling (``$PROFILE`` / exec-policy) is covered
by the Rust ``profiles`` unit tests.
"""

from __future__ import annotations

import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from src.runner import OcxRunner, PackageInfo

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="self setup fence/shim tests assume POSIX sh semantics.",
)

# The install-layout path the bootstrap candidate lives at, relative to OCX_HOME.
_CANDIDATE_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"

# The D10 downgrade diagnostic emits the exact shape ``downgrade <from> -> <to>``
# (ASCII arrow). Match that precise shape, NOT the bare word "downgrade" — the
# word also appears inside generated repo names (e.g. ``..._warn_downgrade``)
# echoed back in unrelated WARN lines, which would make a substring check lie.
_DOWNGRADE_WARN_RE = re.compile(r"downgrade \S+ -> \S+")


def _emits_downgrade_warning(stderr: str) -> bool:
    """True iff stderr carries the actual D10 downgrade diagnostic line."""
    return _DOWNGRADE_WARN_RE.search(stderr) is not None

# The canonical fenced-block body `ocx self setup` writes on POSIX.
#
# Byte-for-byte identical to `ocx_lib::setup::POSIX_BODY`. The block resolves
# OCX_HOME with a `${OCX_HOME:-$HOME/.ocx}` fallback and an existence guard so a
# fresh login shell (where OCX_HOME is not yet exported — env.sh is what exports
# it) never sources `. "/env.sh"` and fails on startup.
_FENCE_BODY = (
    'if [ -f "${OCX_HOME:-$HOME/.ocx}/env.sh" ]; then\n'
    '    . "${OCX_HOME:-$HOME/.ocx}/env.sh"\n'
    "fi"
)

# The five env shims `ocx self setup` writes into $OCX_HOME.
_ENV_SHIMS = ("env.sh", "env.fish", "env.ps1", "env.nu", "env.elv")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _canonical_hash(body: str) -> str:
    """Mirror ``ocx_lib::setup::rc_block::canonical_hash``.

    The opener marker is the low 4 bytes of the SHA-256 of the block body,
    hex-encoded, after normalizing line endings to LF and stripping a single
    trailing newline. Replicated here so a test can seed a fence whose marker
    either matches (ocx-authored) or differs (dirty) from the body on disk.
    """
    unix = body.replace("\r\n", "\n").replace("\r", "\n")
    if unix.endswith("\n"):
        unix = unix[:-1]
    return hashlib.sha256(unix.encode()).digest()[:4].hex()


def _seed_candidate(ocx: OcxRunner) -> Path:
    """Place a real ocx binary as the install candidate under OCX_HOME.

    The offline bootstrap treats an existing install as ``already_present``,
    so seeding the candidate lets the non-bootstrap scenarios run with
    ``--offline`` and no registry.
    """
    candidate = Path(ocx.env["OCX_HOME"]) / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ocx.binary, candidate)
    candidate.chmod(0o755)
    return candidate


def _setup(
    ocx: OcxRunner,
    *extra_args: str,
    profile: Path | None = None,
    home: Path | None = None,
    fmt_json: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx --offline self setup`` with the given extra flags.

    ``--offline`` keeps the bootstrap from touching the network: with a seeded
    candidate it resolves ``already_present``, so the run exercises only the
    shim + profile logic. ``home`` overrides ``$HOME`` so profile auto-detect
    targets an isolated directory.
    """
    cmd = [str(ocx.binary)]
    if fmt_json:
        cmd += ["--format", "json"]
    cmd += ["--offline", "self", "setup", *extra_args]
    if profile is not None:
        cmd += ["--profile", str(profile)]
    env = dict(ocx.env)
    if home is not None:
        env["HOME"] = str(home)
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


# ---------------------------------------------------------------------------
# Fresh setup: shims + fence written
# ---------------------------------------------------------------------------


def test_setup_writes_shims_and_fence(ocx: OcxRunner, tmp_path: Path) -> None:
    """A fresh ``ocx self setup`` writes the five env shims and a v1 fence.

    With a seeded candidate the bootstrap is ``already_present``; the run then
    writes ``$OCX_HOME/env.*`` and splices the managed activation block into the
    target profile. Exit 0, status ``completed``.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    profile.write_text("# existing user content\nexport KEEP=1\n")

    result = _setup(ocx, profile=profile)
    assert result.returncode == 0, (
        f"fresh setup must exit 0; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    payload = json.loads(result.stdout)
    assert payload["status"] == "completed", f"status must be 'completed'; got: {payload!r}"

    # All five env shims must exist under OCX_HOME.
    ocx_home = Path(ocx.env["OCX_HOME"])
    for shim in _ENV_SHIMS:
        assert (ocx_home / shim).is_file(), f"{shim} must be written to OCX_HOME; payload: {payload!r}"

    # The managed v1 fence must be spliced into the profile, with the user's
    # pre-existing content preserved.
    content = profile.read_text()
    expected_marker = _canonical_hash(_FENCE_BODY)
    assert f"# >>> ocx v1 {expected_marker} >>>" in content, (
        f"profile must carry the v1 opener with marker {expected_marker}; got:\n{content}"
    )
    assert _FENCE_BODY in content, f"profile must source env.sh inside the fence; got:\n{content}"
    assert "# <<< ocx <<<" in content, f"profile must carry the fence closer; got:\n{content}"
    assert "export KEEP=1" in content, f"pre-existing user content must survive; got:\n{content}"


def test_setup_env_sh_uses_runtime_ocx_home_not_literal(ocx: OcxRunner) -> None:
    """The ``env.sh`` shim ``ocx self setup`` writes resolves OCX_HOME at runtime.

    The shim must be byte-identical across users: it must NOT embed the literal
    install-time OCX_HOME path and must carry the ``${OCX_HOME:=...}`` runtime
    fallback. (This re-points the former install.sh ``create_env_sh`` assertion
    at the setup-produced file.)
    """
    _seed_candidate(ocx)
    result = _setup(ocx, "--no-modify-path")
    assert result.returncode == 0, f"setup must exit 0; stderr:\n{result.stderr}"

    ocx_home = Path(ocx.env["OCX_HOME"])
    content = (ocx_home / "env.sh").read_text()
    assert "${OCX_HOME:=" in content, (
        f"env.sh must use the ${{OCX_HOME:=...}} runtime fallback; got:\n{content}"
    )
    assert str(ocx_home) not in content, (
        f"env.sh must NOT embed the literal OCX_HOME path ({ocx_home}); got:\n{content}"
    )
    assert "self activate" in content, f"env.sh must delegate to 'ocx self activate'; got:\n{content}"


def test_setup_env_sh_double_source_no_path_duplication(ocx: OcxRunner) -> None:
    """Sourcing ``env.sh`` twice in one session must not duplicate the OCX bin.

    The ``_OCX_ENV_LOADED`` guard wraps the PATH prepend so a re-source (e.g. a
    login shell that sources ``.zprofile`` then ``.zshrc``) is a no-op. This is
    the behavioral check — not just the textual presence of the guard line —
    that the former ``test_install_sh.py`` double-source tests verified. The
    seeded candidate makes ``ocx self activate`` resolve, so the prepend runs.
    """
    _seed_candidate(ocx)
    result = _setup(ocx, "--no-modify-path")
    assert result.returncode == 0, f"setup must exit 0; stderr:\n{result.stderr}"

    ocx_home = Path(ocx.env["OCX_HOME"])
    env_sh = ocx_home / "env.sh"

    # Source env.sh twice in a single bash session, then print the PATH.
    sourced = subprocess.run(
        ["bash", "-c", f'. "{env_sh}"; . "{env_sh}"; printf "%s" "$PATH"'],
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_HOME": str(ocx_home)},
    )
    assert sourced.returncode == 0, (
        f"double-source must exit 0; rc={sourced.returncode}\nstderr:\n{sourced.stderr}"
    )

    # The absolute OCX bin path the activation stream prepends.
    bin_segment = str(ocx_home / "symlinks" / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin")
    occurrences = sourced.stdout.split(":").count(bin_segment)
    assert occurrences == 1, (
        f"the OCX bin path must appear exactly once after double-source "
        f"(the _OCX_ENV_LOADED guard prevents a second prepend); "
        f"found {occurrences} in PATH:\n{sourced.stdout}"
    )


def test_setup_env_shims_byte_identical_across_homes(ocx_binary: Path, registry: str, tmp_path: Path) -> None:
    """The env shims are byte-identical regardless of which OCX_HOME wrote them.

    Re-points the former install.sh ``test_env_sh_byte_identical_across_install_dirs``
    family (sh/fish/ps1) at the setup-produced files. There is NO install-time
    substitution, so every user receives the same shim bytes.
    """
    homes = []
    for name in ("home_a", "home_b"):
        ocx_home = tmp_path / name
        ocx_home.mkdir()
        runner = OcxRunner(ocx_binary, ocx_home, registry)
        _seed_candidate(runner)
        result = _setup(runner, "--no-modify-path")
        assert result.returncode == 0, f"setup ({name}) must exit 0; stderr:\n{result.stderr}"
        homes.append(ocx_home)

    for shim in ("env.sh", "env.fish", "env.ps1"):
        bytes_a = (homes[0] / shim).read_bytes()
        bytes_b = (homes[1] / shim).read_bytes()
        assert bytes_a == bytes_b, (
            f"{shim} must be byte-identical across OCX_HOME dirs (no install-time substitution).\n"
            f"A:\n{bytes_a.decode(errors='replace')}\nB:\n{bytes_b.decode(errors='replace')}"
        )


def test_setup_env_ps1_cross_platform_home_fallback_and_binary_name(ocx: OcxRunner) -> None:
    """The ``env.ps1`` shim ``ocx self setup`` writes carries the cross-platform guards.

    Re-points the former install.sh ``test_env_ps1_cross_platform_home_fallback_and_binary_name``
    and the Gap-D Invoke-Expression guard at the setup-produced file. The
    PowerShell shim must:
      - resolve the OCX_HOME base with a USERPROFILE-with-HOME fallback (Linux
        pwsh has a null ``$env:USERPROFILE``),
      - derive the binary name via the StrictMode-safe ``$env:OS`` probe (never
        ``$IsWindows``, which throws on WinPS 5.1),
      - guard ``Invoke-Expression`` against empty ``self activate`` output.
    """
    _seed_candidate(ocx)
    result = _setup(ocx, "--no-modify-path")
    assert result.returncode == 0, f"setup must exit 0; stderr:\n{result.stderr}"

    content = (Path(ocx.env["OCX_HOME"]) / "env.ps1").read_text()

    assert "$_ocxBase = if ($env:USERPROFILE) { $env:USERPROFILE } else { $HOME }" in content, (
        f"env.ps1 must guard USERPROFILE with a $HOME fallback; got:\n{content}"
    )
    assert "$_ocxExe = if ($env:OS -eq 'Windows_NT') { 'ocx.exe' } else { 'ocx' }" in content, (
        f"env.ps1 must derive the binary name via the $env:OS probe; got:\n{content}"
    )
    assert "$IsWindows" not in content, (
        f"env.ps1 must NOT reference $IsWindows (throws under StrictMode on WinPS 5.1); got:\n{content}"
    )
    assert "| Out-String" in content, f"env.ps1 must collapse activate output via Out-String; got:\n{content}"
    assert "if ($_ocxActivate)" in content, (
        f"env.ps1 must guard Invoke-Expression on the captured non-empty variable; got:\n{content}"
    )


# ---------------------------------------------------------------------------
# Idempotent re-run: no-op
# ---------------------------------------------------------------------------


def test_setup_idempotent_rerun_is_noop(ocx: OcxRunner, tmp_path: Path) -> None:
    """A second ``ocx self setup`` against an unchanged state is a no-op.

    The shims and fence are diff-gated, so the second run reports status
    ``no_op``, writes no shims, and leaves the profile byte-identical. Exit 0.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    profile.write_text("# header\n")

    first = _setup(ocx, profile=profile)
    assert first.returncode == 0, f"first setup must exit 0; stderr:\n{first.stderr}"
    after_first = profile.read_text()

    second = _setup(ocx, profile=profile)
    assert second.returncode == 0, f"second setup must exit 0; stderr:\n{second.stderr}"

    payload = json.loads(second.stdout)
    assert payload["status"] == "no_op", f"re-run must be a no-op; got: {payload!r}"
    assert payload["shims"] == [], f"re-run must write no shims; got: {payload!r}"
    assert profile.read_text() == after_first, "re-run must leave the profile byte-identical"


# ---------------------------------------------------------------------------
# Format-upgrade: stale ocx-authored fence is rewritten
# ---------------------------------------------------------------------------


def test_setup_format_upgrade_rewrites_stale_fence(ocx: OcxRunner, tmp_path: Path) -> None:
    """A v1 fence whose body is ocx-authored but stale is rewritten to canonical.

    Seeds a fence whose opener marker MATCHES its (stale) body hash - proving
    ocx wrote it - but whose body differs from what this binary would write.
    That is the format-upgrade state: the block is rewritten to the canonical
    body + marker, surrounding content preserved, exit 0.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    stale_body = '. "$OCX_HOME/env.sh" # old'
    stale_marker = _canonical_hash(stale_body)
    profile.write_text(
        f"# header\n# >>> ocx v1 {stale_marker} >>>\n{stale_body}\n# <<< ocx <<<\n# tail\n"
    )

    result = _setup(ocx, profile=profile)
    assert result.returncode == 0, (
        f"format-upgrade must exit 0 (ocx-authored, safe to rewrite); rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    content = profile.read_text()
    canonical_marker = _canonical_hash(_FENCE_BODY)
    assert f"# >>> ocx v1 {canonical_marker} >>>" in content, (
        f"stale fence must be rewritten to the canonical marker {canonical_marker}; got:\n{content}"
    )
    assert "# old" not in content, f"stale body must be replaced; got:\n{content}"
    assert "# header" in content and "# tail" in content, (
        f"surrounding content must be preserved; got:\n{content}"
    )


# ---------------------------------------------------------------------------
# Dirty block: exit 82; --force rewrites
# ---------------------------------------------------------------------------


def test_setup_dirty_block_exits_82_and_preserves(ocx: OcxRunner, tmp_path: Path) -> None:
    """A user-edited fence is reported dirty (exit 82) and left untouched.

    The opener marker no longer matches the on-disk body hash (the user added a
    line inside the fence), so the block is classified dirty. Without ``--force``
    setup refuses to overwrite it: exit 82, status ``skipped``, the path listed
    under ``dirty_profiles``, and the profile content preserved verbatim.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    marker = _canonical_hash(_FENCE_BODY)
    # Tamper: an extra line inside the fence so the body hash no longer matches
    # the opener marker.
    tampered = (
        f"# >>> ocx v1 {marker} >>>\n{_FENCE_BODY}\necho tampered\n# <<< ocx <<<\n"
    )
    profile.write_text(tampered)

    result = _setup(ocx, profile=profile)
    assert result.returncode == 82, (
        f"a dirty fence without --force must exit 82; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    payload = json.loads(result.stdout)
    assert payload["status"] == "skipped", f"dirty status must be 'skipped'; got: {payload!r}"
    assert str(profile) in payload["dirty_profiles"], (
        f"dirty profile path must be listed under dirty_profiles; got: {payload!r}"
    )
    assert profile.read_text() == tampered, "a dirty profile must be left untouched (no --force)"


def test_setup_force_rewrites_dirty_block(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--force`` overwrites a dirty fence and exits 0.

    The same tampered fence as above, but with ``--force``: setup rewrites the
    block to the canonical body (the user edit removed), status ``completed``,
    exit 0.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    marker = _canonical_hash(_FENCE_BODY)
    profile.write_text(
        f"# >>> ocx v1 {marker} >>>\n{_FENCE_BODY}\necho tampered\n# <<< ocx <<<\n"
    )

    result = _setup(ocx, "--force", profile=profile)
    assert result.returncode == 0, (
        f"--force must rewrite a dirty fence and exit 0; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    payload = json.loads(result.stdout)
    assert payload["status"] == "completed", f"forced rewrite must be 'completed'; got: {payload!r}"
    content = profile.read_text()
    assert "echo tampered" not in content, f"--force must remove the user edit; got:\n{content}"
    assert f"# >>> ocx v1 {marker} >>>" in content, f"--force must rewrite the canonical fence; got:\n{content}"


# ---------------------------------------------------------------------------
# Legacy migration: # BEGIN ocx / # END ocx -> v1 fence
# ---------------------------------------------------------------------------


def test_setup_migrates_legacy_block(ocx: OcxRunner, tmp_path: Path) -> None:
    """A legacy ``# BEGIN ocx`` / ``# END ocx`` block is migrated to the v1 fence.

    The legacy block (written by the now-deleted installer scaffold) is stripped
    and replaced with the versioned, content-hashed v1 fence in one pass; user
    content outside the block survives. Status ``migrated``, exit 0.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    profile.write_text(
        '# header\n# BEGIN ocx\n. "$OCX_HOME/env.sh"\n# END ocx\nexport KEEP=1\n'
    )

    result = _setup(ocx, profile=profile)
    assert result.returncode == 0, f"legacy migration must exit 0; stderr:\n{result.stderr}"

    payload = json.loads(result.stdout)
    assert payload["status"] == "migrated", f"legacy migration status must be 'migrated'; got: {payload!r}"

    content = profile.read_text()
    assert "# BEGIN ocx" not in content and "# END ocx" not in content, (
        f"legacy block markers must be removed; got:\n{content}"
    )
    marker = _canonical_hash(_FENCE_BODY)
    assert f"# >>> ocx v1 {marker} >>>" in content, f"v1 fence must replace the legacy block; got:\n{content}"
    assert "export KEEP=1" in content, f"user content outside the block must survive; got:\n{content}"


# ---------------------------------------------------------------------------
# --no-modify-path: shims only
# ---------------------------------------------------------------------------


def test_setup_no_modify_path_writes_shims_only(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--no-modify-path`` writes the env shims but touches no profile.

    Even with an explicit ``--profile`` target present on disk, ``--no-modify-path``
    leaves it byte-identical and writes only the shims. The opt-out is not
    remembered between runs (re-running without the flag would write the fence).
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    profile.write_text("# pristine\n")

    result = _setup(ocx, "--no-modify-path", profile=profile)
    assert result.returncode == 0, f"--no-modify-path must exit 0; stderr:\n{result.stderr}"

    payload = json.loads(result.stdout)
    assert payload["profiles"] == [], f"--no-modify-path must touch no profile; got: {payload!r}"
    assert profile.read_text() == "# pristine\n", "the profile must be byte-identical with --no-modify-path"

    ocx_home = Path(ocx.env["OCX_HOME"])
    for shim in _ENV_SHIMS:
        assert (ocx_home / shim).is_file(), f"{shim} must still be written with --no-modify-path"

    # The opt-out is stateless: a second run WITHOUT --no-modify-path must write
    # the fence (no sentinel persisted the first run's opt-out).
    second = _setup(ocx, profile=profile)
    assert second.returncode == 0, f"re-run without --no-modify-path must exit 0; stderr:\n{second.stderr}"
    content = profile.read_text()
    marker = _canonical_hash(_FENCE_BODY)
    assert f"# >>> ocx v1 {marker} >>>" in content, (
        f"re-running without --no-modify-path must write the fence (opt-out is not remembered); got:\n{content}"
    )


@pytest.mark.parametrize("truthy", ["1", "true"])
def test_setup_no_modify_path_via_env_var(ocx: OcxRunner, tmp_path: Path, truthy: str) -> None:
    """A truthy ``OCX_NO_MODIFY_PATH`` env var sets ``--no-modify-path``.

    ``OCX_NO_MODIFY_PATH`` is read through ``ocx_lib::env::flag`` / ``BooleanString``:
    ``1``/``true``/``yes``/``on`` are truthy. A truthy value writes shims only,
    leaving the profile untouched - exactly like the flag. Both ``=1`` and
    ``=true`` are exercised end-to-end so a regression that broke the ``=true``
    spelling at the CLI ``default_value_t = env::flag(..)`` boundary is caught.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    profile.write_text("# pristine\n")

    cmd = [str(ocx.binary), "--format", "json", "--offline", "self", "setup", "--profile", str(profile)]
    env = dict(ocx.env)
    env["OCX_NO_MODIFY_PATH"] = truthy
    result = subprocess.run(cmd, capture_output=True, text=True, env=env)
    assert result.returncode == 0, f"OCX_NO_MODIFY_PATH={truthy} setup must exit 0; stderr:\n{result.stderr}"

    payload = json.loads(result.stdout)
    assert payload["profiles"] == [], (
        f"truthy OCX_NO_MODIFY_PATH={truthy} must skip profile edits; got: {payload!r}"
    )
    assert profile.read_text() == "# pristine\n", (
        f"OCX_NO_MODIFY_PATH={truthy} must leave the profile untouched"
    )

    # All five shims are still written regardless of the truthy spelling.
    ocx_home = Path(ocx.env["OCX_HOME"])
    for shim in _ENV_SHIMS:
        assert (ocx_home / shim).is_file(), f"{shim} must be written with OCX_NO_MODIFY_PATH={truthy}"


# ---------------------------------------------------------------------------
# --dry-run: writes nothing
# ---------------------------------------------------------------------------


def test_setup_dry_run_writes_nothing(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--dry-run`` reports the intended actions but writes nothing.

    No env shims appear under OCX_HOME and the target profile is left
    byte-identical; the report still describes what WOULD be written. Exit 0.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    profile.write_text("# pristine\n")

    result = _setup(ocx, "--dry-run", profile=profile)
    assert result.returncode == 0, f"--dry-run must exit 0; stderr:\n{result.stderr}"

    ocx_home = Path(ocx.env["OCX_HOME"])
    for shim in _ENV_SHIMS:
        assert not (ocx_home / shim).exists(), f"--dry-run must write no {shim}"
    assert profile.read_text() == "# pristine\n", "--dry-run must leave the profile byte-identical"

    # The report still describes what WOULD be written: a no-op-free dry-run
    # reports the would-write shim set and the would-write profile outcome, with
    # the seeded candidate surfacing as bootstrap already_present.
    payload = json.loads(result.stdout)
    assert payload["bootstrap"]["status"] == "already_present", (
        f"a seeded candidate must report bootstrap already_present even on dry-run; got: {payload!r}"
    )
    assert payload["shims"], f"dry-run must report the would-write shim set on a fresh home; got: {payload!r}"
    assert len(payload["profiles"]) == 1, (
        f"dry-run must report the would-write profile outcome; got: {payload!r}"
    )
    assert payload["profiles"][0]["outcome"] == "completed", (
        f"a fresh would-write profile must report outcome 'completed'; got: {payload!r}"
    )


def test_setup_dry_run_with_dirty_profile_exits_zero(ocx: OcxRunner, tmp_path: Path) -> None:
    """``--dry-run`` over a DIRTY fence stays exit 0 (never the dirty exit 82).

    The dry-run short-circuit must intercept BEFORE the dirty-skip branch fires:
    a user-edited fence under ``--dry-run`` is reported as would-skip, never as
    the exit-82 dirty outcome. Seeds the same tampered fence as
    ``test_setup_dirty_block_exits_82_and_preserves`` and asserts the profile is
    left verbatim and no shims are written.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    marker = _canonical_hash(_FENCE_BODY)
    tampered = (
        f"# >>> ocx v1 {marker} >>>\n{_FENCE_BODY}\necho tampered\n# <<< ocx <<<\n"
    )
    profile.write_text(tampered)

    result = _setup(ocx, "--dry-run", profile=profile)
    assert result.returncode == 0, (
        f"--dry-run over a dirty fence must exit 0, not 82; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    assert profile.read_text() == tampered, "--dry-run must leave a dirty profile byte-identical"

    ocx_home = Path(ocx.env["OCX_HOME"])
    for shim in _ENV_SHIMS:
        assert not (ocx_home / shim).exists(), f"--dry-run must write no {shim} even over a dirty profile"

    # The dirty profile is reported as a would-skip in the dry-run report (never
    # the exit-82 dirty outcome): it appears in `profiles` with outcome
    # skipped_dirty, while the top-level status stays the non-skip dry-run shape.
    payload = json.loads(result.stdout)
    assert any(p["outcome"] == "skipped_dirty" for p in payload["profiles"]), (
        f"a dirty profile must appear as a would-skip in the dry-run report; got: {payload!r}"
    )


# ---------------------------------------------------------------------------
# CRLF preservation: a CRLF profile stays CRLF after the fence is spliced
# ---------------------------------------------------------------------------


def test_setup_crlf_profile_stays_crlf(ocx: OcxRunner, tmp_path: Path) -> None:
    """A CRLF-dominant profile keeps CRLF line endings after the fence splice.

    Exercises the full CLI path (spawn_blocking + persist_temp_file), not just
    the unit-level line scanner: a regression that converts CRLF to LF on
    write-back would be invisible to every other acceptance test since none
    present a CRLF profile.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    profile.write_text("# header\r\nexport KEEP=1\r\n")

    result = _setup(ocx, profile=profile)
    assert result.returncode == 0, f"CRLF setup must exit 0; stderr:\n{result.stderr}"

    content = profile.read_bytes()
    assert b"\r\n" in content, f"CRLF line endings must be preserved; got:\n{content!r}"
    assert b"# >>> ocx v1" in content, f"the v1 fence opener must be spliced in; got:\n{content!r}"


# ---------------------------------------------------------------------------
# Forward-version collapse: a v2 fence collapses to a single v1 block
# ---------------------------------------------------------------------------


def test_setup_forward_version_fence_collapses_to_v1(ocx: OcxRunner, tmp_path: Path) -> None:
    """A ``# >>> ocx v2 ... >>>`` fence collapses to a single canonical v1 block.

    A forward-version opener (written by a hypothetical newer binary, body hash
    matching its marker) is ocx-authored, so it is a format upgrade -- NOT a
    dirty edit. The run must rewrite it to a single v1 block at exit 0; a
    regression that treats the v2 opener as dirty (exit 82) would be invisible
    to every other acceptance test.
    """
    _seed_candidate(ocx)
    profile = tmp_path / "profile"
    v2_marker = _canonical_hash(_FENCE_BODY)
    profile.write_text(
        f"# >>> ocx v2 {v2_marker} >>>\n{_FENCE_BODY}\n# <<< ocx <<<\n"
    )

    result = _setup(ocx, profile=profile)
    assert result.returncode == 0, (
        f"a v2 fence must NOT be treated as dirty; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    content = profile.read_text()
    assert "# >>> ocx v1" in content, f"the v2 fence must collapse to a v1 block; got:\n{content}"
    assert "# >>> ocx v2" not in content, f"the v2 opener must be gone after collapse; got:\n{content}"
    assert content.count("# >>> ocx") == 1, f"exactly one fence opener must remain; got:\n{content}"


# ---------------------------------------------------------------------------
# Bootstrap failure: zero shims, zero RC edits
# ---------------------------------------------------------------------------


def test_setup_bootstrap_failure_writes_nothing(ocx: OcxRunner, tmp_path: Path) -> None:
    """When the bootstrap fails, setup writes zero shims and zero profile edits.

    With no candidate installed and the registry redirected to a repo that does
    not exist (the loopback seam, online so the failure is a real registry
    miss), the bootstrap fails FIRST. The hard ordering invariant guarantees no
    env shim and no profile edit happen after a bootstrap failure: the run exits
    non-zero and OCX_HOME carries none of the ``env.*`` files.
    """
    # No candidate seeded: the CAS is empty so bootstrap must resolve a target.
    profile = tmp_path / "profile"
    profile.write_text("# pristine\n")

    cmd = [
        str(ocx.binary),
        "self",
        "setup",
        "--profile",
        str(profile),
    ]
    env = dict(ocx.env)
    # Redirect the canonical self identifier to a guaranteed-absent repo on the
    # loopback registry. Online (no --offline) so the miss is a real failure,
    # not the offline self-heal path.
    env["__OCX_SELF_IMAGE"] = f"{ocx.registry}/nonexistent_self_image_xyz"
    result = subprocess.run(cmd, capture_output=True, text=True, env=env)

    assert result.returncode != 0, (
        f"a failed bootstrap must exit non-zero; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    for shim in _ENV_SHIMS:
        assert not (ocx_home / shim).exists(), (
            f"bootstrap-FIRST invariant: no {shim} must be written after a bootstrap failure"
        )
    assert profile.read_text() == "# pristine\n", (
        "bootstrap-FIRST invariant: no profile edit must happen after a bootstrap failure"
    )


def test_setup_offline_with_no_install_exits_81(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx --offline self setup`` with an empty CAS exits 81 (PolicyBlocked).

    With no candidate seeded and ``--offline`` (the plan-approved seam, already
    passed by the ``_setup`` helper), the bootstrap cannot populate the CAS and
    cannot self-heal to ``already_present`` -- the ``current`` install does not
    resolve. The PolicyBlocked exit code (81) must surface through the full CLI
    stack (bootstrap offline_blocked path -> classify routing). No env shims and
    no profile edits happen (bootstrap-FIRST invariant).
    """
    # No candidate seeded: the CAS is empty so the offline bootstrap has nothing
    # to resolve `current` to, hitting the offline_blocked() path (exit 81).
    profile = tmp_path / "profile"
    profile.write_text("# pristine\n")

    result = _setup(ocx, profile=profile, fmt_json=False)
    assert result.returncode == 81, (
        f"offline + empty CAS must exit 81 (PolicyBlocked); rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    ocx_home = Path(ocx.env["OCX_HOME"])
    for shim in _ENV_SHIMS:
        assert not (ocx_home / shim).exists(), (
            f"no {shim} must be written when the offline bootstrap is blocked"
        )
    assert profile.read_text() == "# pristine\n", (
        "no profile edit must happen when the offline bootstrap is blocked"
    )


# ---------------------------------------------------------------------------
# Success-path bootstrap (registry:2) - installs latest published ocx/cli
#
# Mirrors test_self_update.py::test_self_update_installs_newer_version: the
# `__OCX_SELF_IMAGE` loopback seam redirects the canonical `ocx.sh/ocx/cli`
# identifier to a fixture-published stand-in `ocx` package on localhost:5000.
# The seam is loopback-only-asserted at runtime and compile-gated behind
# `--features ocx/__testing` (test binary is built with it; see test/taskfile.yml).
# ---------------------------------------------------------------------------


def test_setup_bootstrap_pulls_latest_published(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """A fresh ``ocx self setup`` bootstraps the latest published ocx/cli.

    With an empty CAS and the ``__OCX_SELF_IMAGE`` seam pointing at a published
    stand-in ``ocx`` package, the bootstrap installs it (``current`` symlink
    set), then writes the shims. The JSON bootstrap entry reports ``pulled``
    with the published version. Exit 0.
    """
    from src.helpers import make_package

    repo = unique_repo
    # Publish a stand-in `ocx` whose `bin/ocx` answers `--format json version`,
    # exactly as the self-update seam test does.
    pkg = make_package(
        ocx,
        repo,
        "0.0.1",
        tmp_path,
        new=True,
        cascade=False,
        bins=["ocx"],
        outputs={"ocx": {"--format json version": json.dumps({"version": "0.0.1"})}},
    )
    ocx.plain("index", "update", repo)

    cmd = [str(ocx.binary), "--format", "json", "self", "setup", "--no-modify-path"]
    env = dict(ocx.env)
    env["__OCX_SELF_IMAGE"] = f"{ocx.registry}/{repo}"
    result = subprocess.run(cmd, capture_output=True, text=True, env=env)

    assert result.returncode == 0, (
        f"success-path setup must exit 0; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    payload = json.loads(result.stdout)
    assert payload["bootstrap"]["status"] == "pulled", (
        f"a fresh CAS must report bootstrap status 'pulled'; got: {payload!r}"
    )
    assert payload["bootstrap"]["version"] == "0.0.1", (
        f"the pulled version must be the published 0.0.1; got: {payload!r}"
    )

    # `current` now resolves to the installed stand-in (candidate=false, select=true).
    from src.runner import registry_dir

    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / repo
        / "current"
    )
    assert current.is_symlink() or current.exists(), (
        f"bootstrap must select `current` for the pulled ocx/cli; missing at {current}"
    )
    # Keep the package reference alive for the linter.
    assert pkg.tag == "0.0.1"


# ---------------------------------------------------------------------------
# Version-selection tests (plan_self_setup_version_selection.md Phase C.3.2)
#
# Each test covers one UX row from the plan's "User Experience Scenarios" table.
# The `__OCX_SELF_IMAGE` seam redirects `ocx.sh/ocx/cli` to a fixture-published
# stand-in package on localhost:5000.  The seam is loopback-only-asserted at
# runtime and compiled only when built with `--features ocx/__testing`.
#
# Fixture convention: two published versions (v0.9.1 and v0.9.2) let the tests
# exercise pin/downgrade/re-select without needing a real registry.
# ---------------------------------------------------------------------------


def _make_self_packages(
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
) -> tuple[PackageInfo, PackageInfo]:
    """Publish two stand-in ocx/cli versions (v0.9.1 and v0.9.2) into ``repo``.

    Returns ``(pkg_091, pkg_092)``.  Both carry a ``bin/ocx`` that answers
    ``--format json version`` so the bootstrap path can inspect the version.
    """
    from src.helpers import make_package

    pkg091 = make_package(
        ocx,
        repo,
        "0.9.1",
        tmp_path,
        new=True,
        cascade=False,
        bins=["ocx"],
        outputs={"ocx": {"--format json version": json.dumps({"version": "0.9.1"})}},
    )
    pkg092 = make_package(
        ocx,
        repo,
        "0.9.2",
        tmp_path,
        new=False,
        cascade=False,
        bins=["ocx"],
        outputs={"ocx": {"--format json version": json.dumps({"version": "0.9.2"})}},
    )
    return pkg091, pkg092


def _setup_pinned(
    ocx: OcxRunner,
    repo: str,
    version_spec: str,
    *extra_args: str,
    fmt_json: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx self setup <version_spec>`` with the ``__OCX_SELF_IMAGE`` seam.

    Does NOT pass ``--offline`` — pinned setup must reach the local registry
    to resolve and install.  ``--no-modify-path`` keeps profile handling out of
    the scope of version-pin tests.
    """
    # Root resolution flags (e.g. ``--frozen``/``--offline``/``--remote``) must
    # precede the subcommand; ``ocx self setup --frozen`` is a clap usage error.
    # Split ``extra_args`` so root flags go before ``self setup`` and any
    # subcommand flags stay after it.
    _root_flags = {"--frozen", "--offline", "--remote"}
    root_args = [a for a in extra_args if a in _root_flags]
    sub_args = [a for a in extra_args if a not in _root_flags]

    cmd = [str(ocx.binary)]
    if fmt_json:
        cmd += ["--format", "json"]
    cmd += [*root_args, "self", "setup", "--no-modify-path", *sub_args, version_spec]
    env = dict(ocx.env)
    env["__OCX_SELF_IMAGE"] = f"{ocx.registry}/{repo}"
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


# ── UX row: pinned tag install ───────────────────────────────────────────────


def test_pinned_tag_install_reports_version_and_digest(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: `ocx self setup 0.9.2` installs 0.9.2; JSON shows version+digest.

    Plan contract: pinned path resolves the tag, installs with
    ``candidate=false, select=true``, and the `bootstrap` JSON entry carries
    both ``version`` and ``digest`` (non-null).  `current` must resolve.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    result = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert result.returncode == 0, (
        f"pinned tag install must exit 0; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    payload = json.loads(result.stdout)
    bootstrap = payload["bootstrap"]
    assert bootstrap["status"] == "pulled", (
        f"first-time pinned install must report 'pulled'; got: {payload!r}"
    )
    assert bootstrap.get("version") == "0.9.2", (
        f"bootstrap must report the pinned version; got: {payload!r}"
    )
    assert bootstrap.get("digest") is not None, (
        f"bootstrap must report a non-null digest on pinned install; got: {payload!r}"
    )
    assert bootstrap["digest"].startswith("sha256:"), (
        f"digest must be a 'sha256:...' string; got: {payload!r}"
    )

    # A first-time install must NEVER emit a downgrade warning (there is no
    # installed version to compare against; the `installed.is_none()` fast-exit
    # must short-circuit before `maybe_warn_downgrade` can fire).
    assert not _emits_downgrade_warning(result.stderr), (
        f"fresh pinned install must not emit a downgrade warning; got:\n{result.stderr}"
    )

    # `current` must resolve after a pinned install.
    from src.runner import registry_dir
    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "current"
    )
    assert current.is_symlink() or current.exists(), (
        f"pinned install must set `current`; not found at {current}"
    )


# ── UX row: re-run same pin → AlreadyPresent with non-null digest ────────────


def test_rerun_same_pin_is_already_present_with_digest(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: re-running the same pin → `AlreadyPresent`; JSON `digest` non-null.

    Plan contract (D6): satisfied iff `current` already points at the pinned
    digest.  A second run must NOT re-pull (status `already_present`), and the
    digest field must still be set (pinned path always resolves + reports it).
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # First run: pull.
    first = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert first.returncode == 0, (
        f"first pin install must exit 0; rc={first.returncode}\nstderr:\n{first.stderr}"
    )

    # Second run: same pin → already_present with digest still set.
    second = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert second.returncode == 0, (
        f"re-run same pin must exit 0; rc={second.returncode}\nstderr:\n{second.stderr}"
    )

    payload = json.loads(second.stdout)
    bootstrap = payload["bootstrap"]
    assert bootstrap["status"] == "already_present", (
        f"re-run same pin must report 'already_present'; got: {payload!r}"
    )
    assert bootstrap.get("digest") is not None, (
        f"re-run same pin must still carry non-null digest in JSON; got: {payload!r}"
    )


# ── UX row: same pin but `current` re-pointed elsewhere → re-select ──────────


def test_pin_reselects_when_current_moved_elsewhere(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: same pin but `current` was re-pointed elsewhere → re-select (Pulled).

    Plan contract (D6): satisfaction check is whether `current` resolves to
    the pinned digest.  If `current` was moved (e.g. by a concurrent `self update`
    or a different pin), the setup must re-link `current` back to the pinned
    version.  The CAS is not re-downloaded (idempotent install), so this is a
    cheap re-link, reported as `pulled`.
    """
    pkg091, _ = _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # Pin 0.9.2 first so both versions are in the CAS.
    r1 = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert r1.returncode == 0, (
        f"initial 0.9.2 pin must succeed; rc={r1.returncode}\nstderr:\n{r1.stderr}"
    )

    # Move `current` away by pinning 0.9.1.
    r2 = _setup_pinned(ocx, unique_repo, "0.9.1")
    assert r2.returncode == 0, (
        f"0.9.1 pin must succeed; rc={r2.returncode}\nstderr:\n{r2.stderr}"
    )

    # Re-pin 0.9.2 — current was moved away, so this must re-select (not already_present).
    r3 = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert r3.returncode == 0, (
        f"re-pin 0.9.2 after current moved must exit 0; rc={r3.returncode}\nstderr:\n{r3.stderr}"
    )

    payload = json.loads(r3.stdout)
    bootstrap = payload["bootstrap"]
    # The status must be `pulled` (re-linked) rather than `already_present`
    # because `current` was pointing at 0.9.1 at the start of this run.
    assert bootstrap["status"] == "pulled", (
        f"re-pin after current moved must report 'pulled' (re-select); got: {payload!r}"
    )
    assert pkg091.tag == "0.9.1"  # keep reference alive


# ── UX row: downgrade warning on stderr ──────────────────────────────────────


def test_downgrade_warns_on_stderr(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: pinning older after newer installed → stderr warning, exit 0, installed.

    Plan contract (D10): when the pinned tag is semver-older than the currently
    installed `current` version, a single diagnostic line is emitted to stderr.
    The install proceeds (warn-only, not blocked).
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # Install 0.9.2 first so the CAS has a newer current.
    r1 = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert r1.returncode == 0, (
        f"0.9.2 install must succeed; rc={r1.returncode}\nstderr:\n{r1.stderr}"
    )

    # Downgrade to 0.9.1 — must warn on stderr.
    r2 = _setup_pinned(ocx, unique_repo, "0.9.1")
    assert r2.returncode == 0, (
        f"downgrade must exit 0 (warn-only); rc={r2.returncode}\nstderr:\n{r2.stderr}"
    )

    # A downgrade warning must appear on stderr in the precise `downgrade
    # <from> -> <to>` shape (D10). Matching the bare word would be a tautology
    # here: this test's own repo name embeds "downgrade" and is echoed in
    # unrelated WARN lines.
    stderr = r2.stderr
    assert _emits_downgrade_warning(stderr), (
        f"downgrade must warn on stderr in 'downgrade <from> -> <to>' form; got:\n{stderr}"
    )
    match = _DOWNGRADE_WARN_RE.search(stderr)
    assert match is not None and "0.9.2" in match.group() and "0.9.1" in match.group(), (
        f"downgrade warning must name both the current (0.9.2) and target (0.9.1) "
        f"versions; got:\n{stderr}"
    )

    # Downgrade was installed (0.9.1 is now current).
    payload = json.loads(r2.stdout)
    assert payload["bootstrap"]["status"] in ("pulled",), (
        f"downgrade must be installed; got: {payload!r}"
    )


# ── UX row: no downgrade warning when upgrading ──────────────────────────────


def test_upgrade_does_not_warn_downgrade(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: pinning a NEWER version after an older install → no downgrade warning.

    Plan contract (D10): the downgrade warning fires only when the pinned tag is
    semver-older than the currently installed version.  Installing a newer version
    (0.9.1 → 0.9.2) must NOT emit a downgrade warning.  A regression that flips
    the ``<`` comparison in ``maybe_warn_downgrade`` to ``>`` would silently keep
    the existing positive test green while emitting a spurious warning on every
    normal upgrade; this negative assertion closes that gap.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # Install the older version first so a "currently installed" version exists.
    r1 = _setup_pinned(ocx, unique_repo, "0.9.1")
    assert r1.returncode == 0, (
        f"0.9.1 install must succeed; rc={r1.returncode}\nstderr:\n{r1.stderr}"
    )

    # Upgrade to 0.9.2 — must install successfully AND emit no downgrade warning.
    r2 = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert r2.returncode == 0, (
        f"upgrade to 0.9.2 must exit 0; rc={r2.returncode}\nstderr:\n{r2.stderr}"
    )
    assert not _emits_downgrade_warning(r2.stderr), (
        f"upgrading to a newer version must not emit a downgrade warning; got:\n{r2.stderr}"
    )

    payload = json.loads(r2.stdout)
    assert payload["bootstrap"]["status"] in ("pulled",), (
        f"upgrade must be installed (pulled); got: {payload!r}"
    )


# ── UX row: `tag@digest` mismatch → exit 65 ──────────────────────────────────


def test_tag_digest_mismatch_exits_65(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: `tag@digest` mismatch → exit 65 (`DataError`), fail-closed.

    Plan contract (D9): when the tag resolves to a different digest than the
    one pinned in VERSION, the run fails with exit 65.  The error message must
    name both digests so the operator can diagnose whether the index is stale.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # Use a plausible-but-wrong digest — sha256 of the literal string "wrong".
    wrong_hex = "a" * 64  # all-zeros is a plausible but wrong sha256 digest
    wrong_spec = f"0.9.2@sha256:{wrong_hex}"
    result = _setup_pinned(ocx, unique_repo, wrong_spec)

    assert result.returncode == 65, (
        f"tag@digest mismatch must exit 65 (DataError); rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    # The PinDigestMismatch Display must name BOTH the pinned (expected) digest
    # and the resolved (actual) digest so the operator can diagnose whether the
    # index is stale.  The ``or "sha256:"`` form would pass on any sha256 mention,
    # masking a regression where the pinned hex is omitted.  We assert each half
    # independently so a failure message identifies which side is missing.
    stderr = result.stderr
    assert wrong_hex in stderr, (
        f"mismatch error must include the pinned (expected) digest hex; got:\n{stderr}"
    )
    # The resolved digest is any sha256 hex that differs from wrong_hex; we
    # verify it appears as a full ``sha256:<hex>`` token so the format is correct.
    import re as _re
    resolved_digests = _re.findall(r"sha256:([0-9a-f]{64})", stderr)
    assert any(h != wrong_hex for h in resolved_digests), (
        f"mismatch error must include the resolved (actual) digest (a sha256 hex "
        f"different from the pinned wrong_hex); got:\n{stderr}"
    )


# ── UX row: `tag@digest` happy path → exit 0, JSON digest matches ────────────


def test_tag_digest_pin_happy_path(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: `tag@digest` happy path → exit 0, JSON `bootstrap.digest` matches.

    Plan contract (D9): when the tag resolves to the same digest as pinned,
    the install succeeds (exit 0) and ``bootstrap.digest`` in the JSON equals
    the pinned value.  We capture the real digest from an initial tag install,
    then run a fresh install in the same OCX_HOME with the ``tag@digest``
    spec — ``current`` was re-pointed by a 0.9.1 install between runs so the
    second run exercises the install path rather than ``already_present``.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # First install: capture the real digest for 0.9.2.
    prime = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert prime.returncode == 0, (
        f"priming tag install must succeed; rc={prime.returncode}\nstderr:\n{prime.stderr}"
    )
    digest_str: str = json.loads(prime.stdout)["bootstrap"]["digest"]
    assert digest_str and digest_str.startswith("sha256:"), (
        f"priming install must report a sha256 digest; got: {digest_str!r}"
    )

    # Move `current` away (pin 0.9.1) so the tag@digest re-run exercises the
    # install path instead of short-circuiting to already_present.
    reset = _setup_pinned(ocx, unique_repo, "0.9.1")
    assert reset.returncode == 0, (
        f"reset to 0.9.1 must succeed; rc={reset.returncode}\nstderr:\n{reset.stderr}"
    )

    # Re-install with the captured tag@digest spec.
    version_spec = f"0.9.2@{digest_str}"
    result = _setup_pinned(ocx, unique_repo, version_spec)
    assert result.returncode == 0, (
        f"tag@digest happy path must exit 0; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    payload = json.loads(result.stdout)
    bootstrap = payload["bootstrap"]
    assert bootstrap["status"] in ("pulled", "already_present"), (
        f"tag@digest happy path must succeed; got: {payload!r}"
    )
    assert bootstrap.get("digest") == digest_str, (
        f"JSON bootstrap.digest must equal the pinned digest; got: {payload!r}"
    )


# ── UX row: bad VERSION syntax → exit 64 ─────────────────────────────────────


@pytest.mark.parametrize("bad_spec", [
    "1.2.3@",        # trailing @ (no digest after @)
    "@",             # bare @
    f"@sha256:{'a' * 64}",  # leading @ (digest-only pins are written bare)
    f"sha256:{'A' * 64}",   # uppercase hex
    "sha999:abcdef",        # unknown algorithm
])
def test_bad_version_syntax_exits_64(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
    bad_spec: str,
) -> None:
    """UX row: malformed VERSION argument → exit 64 (`UsageError`).

    Plan contract / Error taxonomy: `InvalidVersionSpec` variants (empty string,
    double `@`, trailing `@`, leading `@`, short hex, uppercase hex, unknown
    algorithm) all produce exit 64 via the clap `value_parser`.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    result = _setup_pinned(ocx, unique_repo, bad_spec, fmt_json=False)
    assert result.returncode == 64, (
        f"bad spec {bad_spec!r} must exit 64 (UsageError); rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )


# ── UX row: --dry-run with pin → WouldPull + resolved digest ─────────────────


def test_dry_run_pinned_reports_would_pull_with_digest(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: `--dry-run` with a pinned VERSION → `WouldPull` + digest in JSON.

    Plan dry-run contract: resolution IS performed (read-only), nothing is
    persisted, and the outcome reports `would_pull` with the resolved digest.
    No CAS writes, no `current` symlink.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    result = _setup_pinned(ocx, unique_repo, "0.9.2", "--dry-run")
    assert result.returncode == 0, (
        f"--dry-run pinned must exit 0; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    payload = json.loads(result.stdout)
    bootstrap = payload["bootstrap"]
    assert bootstrap["status"] == "would_pull", (
        f"dry-run pinned must report 'would_pull'; got: {payload!r}"
    )
    assert bootstrap.get("digest") is not None, (
        f"dry-run pinned must report the resolved digest; got: {payload!r}"
    )
    assert bootstrap["digest"].startswith("sha256:"), (
        f"dry-run resolved digest must be a sha256 string; got: {payload!r}"
    )

    # Nothing installed: `current` symlink must not exist.
    from src.runner import registry_dir
    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "current"
    )
    assert not current.exists() and not current.is_symlink(), (
        f"--dry-run must not install anything; `current` should not exist at {current}"
    )


# ── UX row: --dry-run with satisfied pin → AlreadyPresent, not WouldPull ─────


def test_dry_run_pinned_already_present_when_satisfied(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: `--dry-run` on a pin already satisfied → `already_present`, not `would_pull`.

    Plan dry-run contract: when `current` already resolves to the pinned digest
    the bootstrap check is satisfied; a dry-run must report the truthful
    `already_present` status (no-op), NOT `would_pull`.  The digest field must
    be non-null and equal the digest captured from the first real install.
    Nothing on disk must change (the `current` symlink must still resolve after
    the dry-run).
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # Step 1 — real install: pin 0.9.2, capture the digest.
    first = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert first.returncode == 0, (
        f"first pin install must exit 0; rc={first.returncode}\nstderr:\n{first.stderr}"
    )
    first_payload = json.loads(first.stdout)
    captured_digest: str = first_payload["bootstrap"]["digest"]
    assert captured_digest and captured_digest.startswith("sha256:"), (
        f"first install must report a sha256 digest; got: {first_payload!r}"
    )

    # Step 2 — dry-run with same pin: must be already_present, not would_pull.
    result = _setup_pinned(ocx, unique_repo, "0.9.2", "--dry-run")
    assert result.returncode == 0, (
        f"--dry-run on satisfied pin must exit 0; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    payload = json.loads(result.stdout)
    bootstrap = payload["bootstrap"]
    assert bootstrap["status"] == "already_present", (
        f"satisfied pin under --dry-run must report 'already_present' (truthful no-op), "
        f"not 'would_pull'; got: {payload!r}"
    )
    assert bootstrap.get("digest") is not None, (
        f"--dry-run already_present must still carry non-null digest; got: {payload!r}"
    )
    assert bootstrap["digest"] == captured_digest, (
        f"--dry-run already_present digest must equal the installed digest; "
        f"captured={captured_digest!r}, got: {payload!r}"
    )

    # Step 3 — disk unchanged: `current` must still resolve after the dry-run.
    from src.runner import registry_dir

    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "current"
    )
    assert current.is_symlink() or current.exists(), (
        f"--dry-run must not disturb `current`; symlink not found at {current}"
    )


# ── UX row: --frozen with digest pin (cached) works ─────────────────────────


def test_frozen_digest_pin_works_when_cached(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: `--frozen sha256:<cached>` works if blobs cached / index has digest.

    Plan contract (D8): resolution honors ChainMode; `--frozen` + digest pin is
    satisfied from the local index when the manifest is already present.  We
    prime the local index with a regular install first, extract the digest from
    the JSON report, then re-run with `--frozen sha256:<digest>`.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # Step 1: regular install to prime the local index and the CAS blobs.
    prime = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert prime.returncode == 0, (
        f"priming install must succeed; rc={prime.returncode}\nstderr:\n{prime.stderr}"
    )
    digest_str: str = json.loads(prime.stdout)["bootstrap"]["digest"]
    assert digest_str, "priming install must report a digest for the frozen test"

    # Step 2: move `current` away by pinning 0.9.1 so the --frozen digest-pin
    # run exercises the install path rather than short-circuiting to
    # `already_present`.  The CAS still holds the 0.9.2 blobs (content-
    # addressed), so `--frozen` can satisfy the spec without network access.
    reset = _setup_pinned(ocx, unique_repo, "0.9.1")
    assert reset.returncode == 0, (
        f"reset to 0.9.1 must succeed; rc={reset.returncode}\nstderr:\n{reset.stderr}"
    )

    # Step 3: run with --frozen and the full sha256 digest spec.
    # `current` was moved to 0.9.1, so the digest pin must re-select 0.9.2
    # from the cached blobs → status must be `pulled` (re-linked), not
    # `already_present`.  A regression in `--frozen` resolution would either
    # exit non-zero (network blocked) or return `already_present` (wrong
    # satisfaction check), both caught here.
    result = _setup_pinned(ocx, unique_repo, digest_str, "--frozen")
    assert result.returncode == 0, (
        f"--frozen digest pin with cached blobs must exit 0; rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert payload["bootstrap"]["status"] == "pulled", (
        f"--frozen digest pin must re-link (status 'pulled') when current was moved "
        f"away; 'already_present' would mean the satisfaction check ran against 0.9.1; "
        f"got: {payload!r}"
    )


# ── UX row: --frozen tag pin uncached → exit 81 ──────────────────────────────


def test_frozen_tag_pin_uncached_exits_81(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: `--frozen <tag>` with no local-index entry for that tag → exit 81.

    Plan contract (D8): `--frozen` refuses to resolve a tag that is not in
    the local index (unpinned-tag `Op::Resolve` miss → `PolicyResolutionBlocked`,
    exit 81).  We use a tag that was never indexed.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    # `make_package` indexes the tag as part of publishing, so clear the local
    # index (the `tags/` store under OCX_HOME) to recreate the "published but
    # not locally cached" state. `--frozen` must then refuse to resolve the tag.
    tags_dir = Path(ocx.env["OCX_HOME"]) / "tags"
    if tags_dir.exists():
        shutil.rmtree(tags_dir)

    result = _setup_pinned(ocx, unique_repo, "0.9.2", "--frozen", fmt_json=False)
    assert result.returncode == 81, (
        f"--frozen tag pin with uncached tag must exit 81 (PolicyBlocked); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ── UX row: literal `latest` → ordinary tag lookup ───────────────────────────


def test_literal_latest_is_ordinary_tag_lookup(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: `ocx self setup latest` is an ordinary tag lookup (plan D11).

    No special-casing: if the registry publishes a tag named `latest`, it is
    installed; otherwise the run fails with exit 79 (NotFound).  The fixture
    does NOT publish a `latest` tag (cascade=False), so exit 79 is expected.
    This pins the KISS decision: `latest` is not aliased to "omit VERSION".
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    result = _setup_pinned(ocx, unique_repo, "latest", fmt_json=False)
    assert result.returncode == 79, (
        f"literal 'latest' with no such tag must exit 79 (NotFound); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )


# ── UX row: digest-only install → version omitted, digest set ────────────────


def test_digest_only_install_omits_version_field(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: bare `sha256:<hex>` → JSON `version` omitted, `digest` set.

    Plan contract: digest-only pin has no tag to report, so `version` is
    absent from the JSON (skip_serializing_if = None).  `digest` is set.
    We resolve the digest by first doing a tag install, then re-run with
    just the digest.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # Resolve the real digest via a tag install.
    prime = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert prime.returncode == 0, (
        f"priming tag install must succeed; rc={prime.returncode}\nstderr:\n{prime.stderr}"
    )
    digest_str: str = json.loads(prime.stdout)["bootstrap"]["digest"]
    assert digest_str, "priming install must report a digest"

    # Reset `current` by pointing it to a fresh install of 0.9.1 so digest-only
    # triggers a Pulled status.
    reset = _setup_pinned(ocx, unique_repo, "0.9.1")
    assert reset.returncode == 0, (
        f"reset to 0.9.1 must succeed; rc={reset.returncode}\nstderr:\n{reset.stderr}"
    )

    # Install with digest-only spec (bare digest, no `@`).
    result = _setup_pinned(ocx, unique_repo, digest_str)
    assert result.returncode == 0, (
        f"digest-only install must exit 0; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    payload = json.loads(result.stdout)
    bootstrap = payload["bootstrap"]
    # `version` must be absent (digest-only pin has no tag to report).
    assert "version" not in bootstrap, (
        f"digest-only install must NOT include 'version' field; got: {payload!r}"
    )
    assert bootstrap.get("digest") == digest_str, (
        f"digest-only install must carry the digest in JSON; got: {payload!r}"
    )


# ── UX row: digest-only pin never emits a downgrade warning ──────────────────


def test_digest_only_pin_no_downgrade_warning(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """UX row: bare ``sha256:<digest>`` pin after an install must not warn about downgrade.

    Plan contract / ``maybe_warn_downgrade`` spec: when the VERSION spec has no
    tag component (``spec.tag() is None``), the downgrade check is skipped
    entirely.  A regression that removed the ``spec.tag() is None`` guard would
    cause every digest-only pin to spuriously warn or panic on a version
    comparison.  This assertion closes that gap.

    Scenario: install 0.9.2 via tag (so a "current" version is recorded), then
    pin the same version by bare digest only.  No version comparison is possible
    (no tag in the spec), so stderr must contain no downgrade language.
    """
    _make_self_packages(ocx, unique_repo, tmp_path)
    ocx.plain("index", "update", unique_repo)

    # Step 1: tag install to record a "current" version and capture the digest.
    prime = _setup_pinned(ocx, unique_repo, "0.9.2")
    assert prime.returncode == 0, (
        f"priming tag install must succeed; rc={prime.returncode}\nstderr:\n{prime.stderr}"
    )
    digest_str: str = json.loads(prime.stdout)["bootstrap"]["digest"]
    assert digest_str, "priming install must report a digest for the digest-only pin test"

    # Step 2: move `current` away so the digest-only pin runs the install path.
    reset = _setup_pinned(ocx, unique_repo, "0.9.1")
    assert reset.returncode == 0, (
        f"reset to 0.9.1 must succeed; rc={reset.returncode}\nstderr:\n{reset.stderr}"
    )

    # Step 3: pin by bare digest only — no tag in spec → no downgrade check.
    result = _setup_pinned(ocx, unique_repo, digest_str)
    assert result.returncode == 0, (
        f"digest-only pin must exit 0; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert not _emits_downgrade_warning(result.stderr), (
        f"digest-only pin (no tag) must not emit a downgrade warning; got:\n{result.stderr}"
    )
