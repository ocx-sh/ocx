# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx self update` and `ocx self update --check`.

These tests exercise both the `ocx --format json version` contract that
`query_installed_version` depends on, and the end-to-end self-update install
path via the private `__OCX_SELF_IMAGE` test-only seam (URI-1).

The seam is gated behind the `__testing` Cargo feature in `ocx_lib` and
`ocx_cli`. The test binary is built with that feature enabled (see
`test/taskfile.yml::build`). The seam carries a runtime loopback-only
assertion so even with the feature compiled in, only `localhost` /
`127.0.0.1` / `[::1]` registries are accepted.

In release builds the seam is compile-gated out entirely — the code path is
not present in shipped binaries.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from src import (
    OcxRunner,
    assert_not_exists,
    assert_symlink_exists,
    make_package,
    registry_dir,
)


# ---------------------------------------------------------------------------
# `ocx version` JSON contract
# ---------------------------------------------------------------------------


def test_version_json_format(ocx: OcxRunner) -> None:
    """``ocx --format json version`` must return valid JSON with a ``version``
    field whose value matches the plain ``ocx version`` output.

    This is the contract that `query_installed_version` in
    `crates/ocx_lib/src/package_manager/tasks/update_check.rs` relies on when
    it invokes the installed binary to determine the running version.
    """
    # JSON form via OcxRunner.json (prepends --format json).
    json_result = ocx.json("version")

    assert "version" in json_result, (
        f"`ocx --format json version` must return an object with a 'version' key; got: {json_result!r}"
    )
    version_from_json = json_result["version"]
    assert isinstance(version_from_json, str), (
        f"version field must be a string; got: {type(version_from_json).__name__!r}"
    )
    assert version_from_json, "version field must not be empty"

    # Plain form — strip trailing whitespace so the comparison is exact.
    plain_result = ocx.plain("version")
    version_from_plain = plain_result.stdout.strip()

    assert version_from_json == version_from_plain, (
        f"`ocx --format json version` and `ocx version` must report the same version string; "
        f"json={version_from_json!r}, plain={version_from_plain!r}"
    )

    # GAP-4: `ocx version` plain output is a single-line bare semver string.
    # Scripts and piped consumers (`VERSION=$(ocx version)`) rely on this; a
    # multi-line plain output would silently break those consumers.
    plain_lines = [line for line in plain_result.stdout.splitlines() if line.strip()]
    assert len(plain_lines) == 1, (
        f"`ocx version` plain output must be exactly one line (script-consumer contract); "
        f"got {len(plain_lines)} lines: {plain_result.stdout!r}"
    )


def test_version_json_shape(ocx: OcxRunner) -> None:
    """``ocx --format json version`` must always carry a parseable
    ``version`` field whose value is a non-empty semver-shaped string.

    Pins the wire-format invariant the subprocess consumer
    (`query_installed_version`, in
    `crates/ocx_lib/src/package_manager/tasks/update_check.rs`) relies
    on: it parses ``.get("version")`` and feeds the value to
    ``semver::Version::parse``. Any additional top-level keys
    (``cargo_pkg_version``, ``channel``, ``commit``, ``build``, ``ci``)
    are additive build provenance that this test deliberately tolerates
    — they appear in dev-deploy / CI builds but never in local
    ``cargo build`` runs without git.
    """
    json_result = ocx.json("version")

    assert "version" in json_result, (
        f"JSON version output must always contain a 'version' key; got keys: {set(json_result.keys())!r}"
    )
    version = json_result["version"]
    assert isinstance(version, str) and version, (
        f"'version' must be a non-empty string; got: {version!r}"
    )
    # Enriched fields, when present, must keep their declared shape — a
    # regression that emits a string where an object is expected would
    # break downstream bug-report tooling.
    for nested_key in ("commit", "build", "ci"):
        if nested_key in json_result:
            assert isinstance(json_result[nested_key], dict), (
                f"'{nested_key}' must be a JSON object when present; got: {type(json_result[nested_key]).__name__}"
            )


def test_version_json_under_env_clear(ocx: OcxRunner) -> None:
    """``ocx --format json version`` must succeed and emit a populated
    ``version`` field when the process environment is fully cleared.

    Simulates the exact subprocess invocation shape used by
    ``query_installed_version`` in
    ``crates/ocx_lib/src/package_manager/tasks/update_check.rs``,
    which calls ``Command::env_clear()`` before spawning the binary.
    The provenance fields (``commit``, ``build``, ``ci``) are baked at
    compile time via ``option_env!()`` (see
    ``crates/ocx_cli/src/app/build_info.rs:14-22``); this test pins the
    hermetic-subprocess invariant so a future regression that reads
    ``std::env`` at runtime breaks visibly.

    A minimal ``HOME`` is injected because some TLS backends on Linux
    probe ``$HOME/.netrc`` or the system certificate store via a path
    that may ultimately need a writable home; ``version`` itself is
    purely in-process but the binary init path on some platforms queries
    HOME for config-dir resolution before the command dispatch occurs.
    The HOME value points at a non-existent directory so no host
    configuration leaks in.
    """
    result = subprocess.run(
        [str(ocx.binary), "--format", "json", "version"],
        capture_output=True,
        text=True,
        env={"HOME": "/nonexistent"},
    )
    assert result.returncode == 0, (
        f"`ocx --format json version` must succeed under env_clear(); "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise AssertionError(
            f"`ocx --format json version` must emit valid JSON under env_clear(); "
            f"stdout={result.stdout!r}"
        ) from exc
    assert isinstance(payload.get("version"), str) and payload["version"], (
        f"version field must be a non-empty string under env_clear(); "
        f"got: {payload!r}"
    )


# ---------------------------------------------------------------------------
# URI-1 — End-to-end self-update install path via `__OCX_SELF_IMAGE` seam
#
# Exercises the full `SelfUpdateResult::Installed { from, to }` path against
# a real OCI registry on localhost:5000 by redirecting the canonical
# `ocx.sh/ocx/cli` identifier to a test repo via the private seam.
#
# Builds two versions of a stand-in "ocx" package (a shell script that
# responds correctly to `ocx --format json version`), pre-installs the older
# one, then runs `ocx self update` with the seam active and asserts:
#   - exit code 0
#   - `current` symlink updated to the newer version
#   - NO `candidates/<version>` symlink (decision: `candidate=false`)
#   - JSON wire shape `{"status":"installed","from":"0.0.1","to":"0.0.2"}`
#
# The test is Linux/macOS only — the seam exists on every platform but the
# stand-in binary is a POSIX shell script (Windows requires a .bat shim).
# ---------------------------------------------------------------------------


# URI-1 is POSIX-only because the stand-in `ocx` package is a shell script.
# The seam itself is cross-platform; only the test harness is sh-bound.
_skip_on_windows = pytest.mark.skipif(
    sys.platform == "win32",
    reason="URI-1 stand-in package uses a POSIX shell script; Windows not covered here.",
)


# The five per-shell env shims `ocx self setup` / the 4C refresh hook writes
# into $OCX_HOME (mirrors `_ENV_SHIMS` in test_self_setup.py).
_ENV_SHIMS = ("env.sh", "env.fish", "env.ps1", "env.nu", "env.elv")


def _publish_two_versions(ocx: OcxRunner, repo: str, tmp_path: Path) -> None:
    """Publish stand-in ``<repo>:0.0.1`` and ``<repo>:0.0.2`` and index them.

    Each stand-in ``bin/ocx`` answers ``--format json version`` with its own
    version so ``query_installed_version`` can resolve the running version.
    """
    make_package(
        ocx, repo, "0.0.1", tmp_path,
        new=True, cascade=False, bins=["ocx"],
        outputs={"ocx": {"--format json version": json.dumps({"version": "0.0.1"})}},
    )
    make_package(
        ocx, repo, "0.0.2", tmp_path,
        new=False, cascade=False, bins=["ocx"],
        outputs={"ocx": {"--format json version": json.dumps({"version": "0.0.2"})}},
    )
    ocx.plain("index", "update", repo)


def _run_self_update_via_seam(
    ocx: OcxRunner, repo: str, *extra_flags: str
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx --format json [extra_flags] self update`` with the ``__OCX_SELF_IMAGE`` seam."""
    env = dict(ocx.env)
    env["__OCX_SELF_IMAGE"] = f"{ocx.registry}/{repo}"
    return subprocess.run(
        [str(ocx.binary), "--format", "json", *extra_flags, "self", "update"],
        capture_output=True,
        text=True,
        env=env,
    )


def _curate_local_index_to_v1(custom_index: Path) -> None:
    """Drop the ``0.0.2`` tag from the local index so it blesses only ``0.0.1``.

    Diverges the local index from the registry (which still holds both tags) so
    a test can prove whether a code path consults the local index or the
    registry for tag discovery.
    """
    idx_file = next(custom_index.rglob("*.json"))
    data = json.loads(idx_file.read_text())
    data["tags"].pop("0.0.2", None)
    idx_file.write_text(json.dumps(data))


@_skip_on_windows
def test_self_update_default_mode_respects_local_index(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """Default-mode ``ocx self update`` discovers the latest version through the
    local index (``OCX_INDEX``), not a live registry probe.

    Regression for the OCX_INDEX-ignored bug: version discovery used to build a
    throwaway remote-only index (`check_update` → `Index::from_remote`), so it
    saw registry tags the curated local index deliberately omitted — every other
    tag-resolving command honours the local index in default mode, but self
    update did not. With the local index blessing only ``0.0.1`` while the
    registry also holds ``0.0.2``, default-mode self update must report
    ``up_to_date`` (honouring OCX_INDEX), never jump to ``0.0.2``.
    """
    repo = unique_repo
    custom_index = tmp_path / "custom_index"
    custom_index.mkdir()
    ocx.env["OCX_INDEX"] = str(custom_index)

    _publish_two_versions(ocx, repo, tmp_path)  # indexes BOTH tags into custom_index
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")
    _curate_local_index_to_v1(custom_index)

    result = _run_self_update_via_seam(ocx, repo)
    assert result.returncode == 0, (
        f"self update must exit 0; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert payload.get("status") == "up_to_date", (
        f"default-mode self update must honour the curated local index (only 0.0.1 blessed) and report "
        f"up_to_date, not install the registry-only 0.0.2; got: {payload!r}"
    )


@_skip_on_windows
def test_self_update_remote_mode_queries_registry(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """``ocx --remote self update`` forces a live registry probe, finding versions
    the local index omits.

    The escape hatch for the default-mode local-index behaviour: a user who wants
    the freshest upstream release passes ``--remote`` (consistent with every other
    tag-resolving command). With the local index blessing only ``0.0.1`` but the
    registry holding ``0.0.2``, ``--remote`` must install ``0.0.2``.
    """
    repo = unique_repo
    custom_index = tmp_path / "custom_index"
    custom_index.mkdir()
    ocx.env["OCX_INDEX"] = str(custom_index)

    _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")
    _curate_local_index_to_v1(custom_index)

    result = _run_self_update_via_seam(ocx, repo, "--remote")
    assert result.returncode == 0, (
        f"--remote self update must exit 0; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert payload.get("status") == "installed" and payload.get("to") == "0.0.2", (
        f"--remote self update must query the registry and install 0.0.2; got: {payload!r}"
    )


@_skip_on_windows
def test_self_update_refreshes_env_shims(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """After ``self update`` installs a newer version, the env.* shims exist.

    Decision 4C behavior 1: a successful binary swap calls
    ``setup::shims::refresh_shims``, which writes the five ``$OCX_HOME/env.*``
    loader files (diff-gated). On a fresh OCX_HOME none exist before the update,
    so all five must be present afterwards.
    """
    repo = unique_repo
    _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")

    ocx_home = Path(ocx.env["OCX_HOME"])
    for shim in _ENV_SHIMS:
        assert not (ocx_home / shim).exists(), f"{shim} must not exist before the update"

    result = _run_self_update_via_seam(ocx, repo)
    assert result.returncode == 0, (
        f"self update must exit 0; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert json.loads(result.stdout).get("status") == "installed", (
        f"self update must report status='installed'; got: {result.stdout!r}"
    )

    for shim in _ENV_SHIMS:
        assert (ocx_home / shim).is_file(), f"4C refresh must write {shim} after the binary swap"


@_skip_on_windows
def test_self_update_advisory_fires_on_shim_drift(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """The ``run 'ocx self setup'`` advisory fires when a shim drifted.

    Decision 4C behavior 3: pre-seed a stale ``env.sh`` whose bytes differ from
    the canonical shim. The 4C refresh hook diff-gate detects the drift, rewrites
    the shim, and emits the advisory on stderr.
    """
    repo = unique_repo
    _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")

    ocx_home = Path(ocx.env["OCX_HOME"])
    ocx_home.mkdir(parents=True, exist_ok=True)
    (ocx_home / "env.sh").write_text("# stale shim content that will be rewritten\n")

    result = _run_self_update_via_seam(ocx, repo)
    assert result.returncode == 0, (
        f"self update must exit 0; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "ocx self setup" in result.stderr, (
        f"a drifted shim must trigger the 'run ocx self setup' advisory on stderr; got:\n{result.stderr}"
    )


@_skip_on_windows
def test_self_update_does_not_touch_user_rc(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """``self update`` never modifies a user shell profile.

    Decision 4C behavior 2: the 4C refresh hook touches only the ocx-owned
    ``$OCX_HOME/env.*`` shims; it must leave any user RC profile byte-identical.
    """
    repo = unique_repo
    _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")

    profile = tmp_path / "user_profile"
    original = "# user content\nexport KEEP=1\n. \"$OCX_HOME/env.sh\"\n"
    profile.write_text(original)

    result = _run_self_update_via_seam(ocx, repo)
    assert result.returncode == 0, (
        f"self update must exit 0; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    assert profile.read_text() == original, "self update must leave the user RC profile byte-identical"


@_skip_on_windows
def test_self_update_installs_newer_version(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """End-to-end: ``ocx self update`` upgrades the installed version when a
    newer tag exists in the registry.

    Sequence:
      1. Publish `<repo>:0.0.1` and `<repo>:0.0.2` to localhost:5000.
      2. Pre-install `<repo>:0.0.1` with `--select` so `current` points at it.
      3. Set `__OCX_SELF_IMAGE=localhost:5000/<repo>` and run `ocx self update`
         with `--format json` so the JSON wire shape can be asserted.
      4. Assert exit 0.
      5. Assert JSON output is `{"status":"installed","from":"0.0.1","to":"0.0.2"}`.
      6. Assert `current` symlink now resolves to the 0.0.2 content directory.
      7. Assert NO `candidates/0.0.2` symlink (decision: `candidate=false`).

    The seam is loopback-only (asserted at runtime inside `ocx_cli_identifier`);
    `localhost:5000` satisfies that gate.  The seam itself is compile-gated
    behind `--features __testing`.
    """
    # Use the same `unique_repo` for both publish and seam so identifiers match.
    repo = unique_repo

    # 1. Publish 0.0.1 and 0.0.2 — cascade=False to avoid latest/major/minor
    #    cascade churn; we only need the two patch tags discoverable in the
    #    remote tag list.
    v1 = make_package(
        ocx, repo, "0.0.1", tmp_path,
        new=True, cascade=False, bins=["ocx"],
        outputs={"ocx": {"--format json version": json.dumps({"version": "0.0.1"})}},
    )
    v2 = make_package(
        ocx, repo, "0.0.2", tmp_path,
        new=False, cascade=False, bins=["ocx"],
        outputs={"ocx": {"--format json version": json.dumps({"version": "0.0.2"})}},
    )
    # Make both tags visible in the index for self_check_update's tag walk.
    ocx.plain("index", "update", repo)

    # 2. Pre-install 0.0.1 with --select so `current` points to it.
    #    This populates the local store so `query_installed_version` can
    #    resolve the running version via the env-composed PATH lookup.
    ocx.json("package", "install", "-s", v1.short)

    current_symlink = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / repo
        / "current"
    )
    assert_symlink_exists(current_symlink)

    # 3. Activate the seam and run `ocx --format json self update`.
    env = dict(ocx.env)
    env["__OCX_SELF_IMAGE"] = f"{ocx.registry}/{repo}"

    result = subprocess.run(
        [str(ocx.binary), "--format", "json", "self", "update"],
        capture_output=True,
        text=True,
        env=env,
    )

    # 4. Exit code 0.
    assert result.returncode == 0, (
        f"`ocx self update` must exit 0 when a newer version is installed; "
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    # 5. JSON wire shape.
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as e:
        raise AssertionError(
            f"`ocx --format json self update` must produce valid JSON; "
            f"got stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        ) from e
    assert payload.get("status") == "installed", (
        f"JSON payload must have status='installed' when an update was installed; "
        f"got: {payload!r}"
    )
    assert payload.get("from") == "0.0.1", (
        f"JSON payload must report from='0.0.1'; got: {payload!r}\n"
        "If this is None, query_installed_version returned None — the "
        "stand-in `bin/ocx` script did not return the expected JSON for "
        "`--format json version`."
    )
    assert payload.get("to") == "0.0.2", (
        f"JSON payload must report to='0.0.2'; got: {payload!r}"
    )

    # 6. `current` symlink now points to 0.0.2 content.
    #    install_all(candidate=false, select=true) updates `current` only.
    assert current_symlink.is_symlink() or current_symlink.exists(), (
        f"current symlink must still exist after self update; missing at {current_symlink}"
    )
    # Resolve current → package-root directory of v2 (post-install symlink
    # targets the package root, not the content tree). Package files live
    # under `<resolved>/content/` per the three-tier CAS layout.
    resolved = current_symlink.resolve()
    bin_ocx = resolved / "content" / "bin" / "ocx"
    assert bin_ocx.exists(), (
        f"current symlink must resolve to a package root containing content/bin/ocx; "
        f"resolved={resolved}, content: {list(resolved.iterdir()) if resolved.exists() else 'absent'}"
    )
    # The v2 marker presence is the strongest assertion that the new version
    # is what `current` resolves to.  `content/bin/ocx` for v2 returns
    # {"version":"0.0.2"} when invoked with --format json version.
    if bin_ocx.exists():
        probe = subprocess.run(
            [str(bin_ocx), "--format", "json", "version"],
            capture_output=True,
            text=True,
            env={"PATH": "/usr/bin:/bin"},
        )
        # The stand-in trap script ignores args except the literal "--format
        # json version" match; on success its stdout should be the JSON shape.
        if probe.returncode == 0:
            try:
                probe_payload = json.loads(probe.stdout)
                assert probe_payload.get("version") == "0.0.2", (
                    f"current/bin/ocx must report version 0.0.2 after self update; "
                    f"got: {probe_payload!r}"
                )
            except json.JSONDecodeError:
                # The stand-in script's fallback echoes a marker — not JSON.
                # That means current points at the old version (test failure).
                raise AssertionError(
                    f"current/bin/ocx did not return JSON for `--format json version`; "
                    f"stdout: {probe.stdout!r}"
                )

    # 7. NO `candidates/0.0.2` symlink — self-update sets candidate=false.
    candidate_v2 = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / repo
        / "candidates"
        / "0.0.2"
    )
    assert_not_exists(candidate_v2)

    # Keep v2 reference alive for the duration of the test (silences linter).
    assert v2.tag == "0.0.2"
