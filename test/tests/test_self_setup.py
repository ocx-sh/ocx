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
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from src.runner import OcxRunner

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="self setup fence/shim tests assume POSIX sh semantics.",
)

# The install-layout path the bootstrap candidate lives at, relative to OCX_HOME.
_CANDIDATE_REL = Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"

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
