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

import hashlib
import json
import shlex
import subprocess
import sys
from pathlib import Path
from typing import Literal

import pytest

from src import (
    OcxRunner,
    assert_not_exists,
    assert_symlink_exists,
    current_platform,
    make_package,
    registry_dir,
)
from src.registry import fetch_platform_manifest_digest

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
        env={"HOME": "/nonexistent"}, check=False,
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
# Two versions of a stand-in "ocx" package are published to the loopback
# registry and the canonical `ocx.sh/ocx/cli` identifier is redirected onto
# them through the private seam, so the whole update runs against real OCI
# content with the older version pre-installed.
#
# `ocx self update` pulls **without selecting** and then re-executes the newly
# pulled binary as `ocx self setup <tag>@<digest> --handoff`; that child's
# first phase performs the select and its later phases write every setup
# surface with the new version's own code. The 0.0.2 stand-in is therefore not
# a passive trap script — it records what it was handed (see `_stand_in_ocx`)
# and then either re-execs the real ocx so the whole setup runs, or refuses.
# The recorded receipt is what tells "the child did the work" apart from "the
# parent did it", which no observable on the finished machine can.
#
# The tests are Linux/macOS only — the seam exists on every platform but the
# stand-in binary is a POSIX shell script (Windows requires a .bat shim).
# ---------------------------------------------------------------------------


# URI-1 is POSIX-only because the stand-in `ocx` package is a shell script.
# The seam itself is cross-platform; only the test harness is sh-bound.
_skip_on_windows = pytest.mark.skipif(
    sys.platform == "win32",
    reason="URI-1 stand-in package uses a POSIX shell script; Windows not covered here.",
)


# The five per-shell env shims `ocx self setup` writes into $OCX_HOME (mirrors
# `_ENV_SHIMS` in test_self_setup.py).
_ENV_SHIMS = ("env.sh", "env.fish", "env.ps1", "env.nu", "env.elv")

# Where each platform keeps its session-PATH store, spelled the way
# `ocx_lib::setup::session_path::stores_for` spells it: the environment
# variable to root the path at, and the path relative to it.
#
# Presence of this file after an update is evidence *about the hand-off*. Only
# a full `setup::run` reaches phase 3.5 and writes it, and the code this WP
# replaced ran a two-phase subset (shims + profile heal) out of the OLD binary
# that had never heard of the session-PATH stores — which is exactly why a
# machine updated 0.6.0 -> 0.6.1 came out the other side with none.
_SESSION_PATH_STORES = {
    "linux": ("XDG_CONFIG_HOME", Path("environment.d") / "ocx.conf"),
    "darwin": ("HOME", Path("Library") / "LaunchAgents" / "sh.ocx.path.plist"),
}

# What the "refuse" stand-in exits with. Any non-zero value that is not one ocx
# assigns a meaning to: the point is that the parent reports it verbatim rather
# than recognising it.
_HANDOFF_REFUSAL_CODE = 70

# The POSIX fence body `ocx self setup` manages, byte-for-byte identical to
# `ocx_lib::setup::POSIX_BODY`. Restated here rather than imported from
# test_self_setup.py: a test module that seeds a fence has to know what a real
# one looks like, and a cross-module import would make one suite's fixture a
# dependency of the other's.
_FENCE_BODY = (
    'if [ -f "${OCX_HOME:-$HOME/.ocx}/env.sh" ]; then\n'
    '    . "${OCX_HOME:-$HOME/.ocx}/env.sh"\n'
    "fi"
)

# The second line of `ocx_lib::setup::shims::ENV_SH`. Enough to tell ocx's own
# shim body apart from whatever a drift test seeded, and short enough that a
# reformat of the shim does not silently stop matching.
_CANONICAL_SHIM_HEADER = "# Managed by ocx installer - do not edit."

# The parent's `HandoffAdvisory::Reload` wording, verbatim from
# `self_group/update.rs::emit_advisory`. Reachable ONLY from
# `Installed { handoff: None }` — every other outcome has its own wording — so
# it is the one stderr string that discriminates a completed hand-off. Never
# assert a bare "ocx self setup" instead: three different advisories say that.
_RELOAD_ADVISORY = "shell integration refreshed"


def _canonical_hash(body: str) -> str:
    """Mirror ``ocx_lib::setup::rc_block::canonical_hash``.

    The opener marker is the low 4 bytes of the SHA-256 of the block body, hex
    encoded, after normalizing line endings to LF and stripping one trailing
    newline. Reproducing it is what lets a test seed a fence whose marker
    *matches* its body — the state machine then reads the extra line inside the
    fence as a user edit (dirty) rather than as drift (heal).
    """
    unix = body.replace("\r\n", "\n").replace("\r", "\n").removesuffix("\n")
    return hashlib.sha256(unix.encode()).digest()[:4].hex()


def _session_path_store(env: dict[str, str]) -> Path | None:
    """This platform's session-PATH store under ``env``, or ``None`` if unmodelled."""
    modelled = _SESSION_PATH_STORES.get(sys.platform)
    if modelled is None:
        return None
    root, relative = modelled
    return Path(env[root]) / relative


def _current_symlink(ocx: OcxRunner, repo: str) -> Path:
    """The ``current`` install symlink for ``repo`` under the runner's OCX_HOME."""
    return Path(ocx.env["OCX_HOME"]) / "symlinks" / registry_dir(ocx.registry) / repo / "current"


def _stand_in_ocx(
    version: str,
    *,
    receipt: Path,
    current: Path,
    delegate_to: Path | None,
) -> str:
    """The ``bin/ocx`` body of a stand-in ocx release.

    Two jobs, and the second is what makes the hand-off observable:

    * ``--format json version`` answers with ``version``, so
      ``query_installed_version`` can read the running version out of the
      installed package.
    * every other invocation is the hand-off ``ocx self update`` spawns
      (``self setup <tag>@<digest> --handoff``). It appends a **receipt** — the
      argv it was handed, and the package ``current`` named at the moment it
      started — and then either re-executes the real ocx binary, so the new
      version's own setup really runs (select included), or refuses.

    The receipt is the surface only the child can write. The parent never runs
    this script, so a receipt existing at all proves the hand-off happened; and
    its ``current=`` line proves the parent had *not* already performed the
    select, which is the half a "the store is there afterwards" assertion
    cannot tell apart.

    The delegated child's stdout is redirected to stderr. `run_handoff` inherits
    standard I/O, so a real child's plain setup table lands on the parent's
    stdout in front of the JSON document ``ocx --format json self update`` is
    asked for. That belongs to the product, not to this fixture — keeping the
    fixture's data stream clean is what lets these tests assert the parent's own
    wire shape at all.
    """
    tail = (
        f'exec {shlex.quote(str(delegate_to))} "$@" 1>&2'
        if delegate_to is not None
        else f"exit {_HANDOFF_REFUSAL_CODE}"
    )
    return (
        "#!/bin/sh\n"
        f"# Stand-in ocx {version} - see `_stand_in_ocx` in test_self_update.py.\n"
        'if [ "$1" = "--format" ] && [ "$2" = "json" ] && [ "$3" = "version" ]; then\n'
        f"""    printf '%s\\n' '{json.dumps({"version": version})}'\n"""
        "    exit 0\n"
        "fi\n"
        "{\n"
        """    printf 'argv=%s\\n' "$*"\n"""
        f"""    printf 'current=%s\\n' "$(cd -P {shlex.quote(str(current))} 2>/dev/null && pwd -P"""
        """ || printf '(absent)')"\n"""
        f"}} >> {shlex.quote(str(receipt))}\n"
        f"{tail}\n"
    )


def _publish_two_versions(
    ocx: OcxRunner,
    repo: str,
    tmp_path: Path,
    *,
    hand_off: Literal["delegate", "refuse"] = "delegate",
) -> Path:
    """Publish stand-in ``<repo>:0.0.1`` and ``<repo>:0.0.2``, index both, and
    return the path the 0.0.2 stand-in writes its hand-off receipt to.

    ``hand_off`` selects what the 0.0.2 stand-in does when it is handed the
    setup — ``"delegate"`` re-execs the real ocx so the select and every later
    phase happen for real, ``"refuse"`` records the receipt and exits
    :data:`_HANDOFF_REFUSAL_CODE` having selected nothing.

    ``0.0.1`` stays a plain trap script; it is only ever asked for its version.
    """
    receipt = tmp_path / "handoff-receipt.txt"
    make_package(
        ocx, repo, "0.0.1", tmp_path, cascade=False, bins=["ocx"],
        outputs={"ocx": {"--format json version": json.dumps({"version": "0.0.1"})}},
    )
    make_package(
        ocx, repo, "0.0.2", tmp_path, cascade=False, bins=["ocx"],
        bin_scripts={
            "ocx": _stand_in_ocx(
                "0.0.2",
                receipt=receipt,
                current=_current_symlink(ocx, repo),
                delegate_to=ocx.binary if hand_off == "delegate" else None,
            )
        },
    )
    ocx.plain("index", "update", repo)
    return receipt


def _hand_off_receipt(receipt: Path) -> dict[str, str]:
    """Parse the record the hand-off child wrote, naming its absence precisely.

    An absent receipt means the child never started. Every outcome assertion
    downstream would otherwise read that as some other cause — a select that
    did not happen, a store that was not written — so it is called out here.
    """
    assert receipt.is_file(), (
        f"the hand-off child never ran: no receipt at {receipt}. `self update` must "
        "re-execute the newly pulled binary as `ocx self setup <tag>@<digest> --handoff`"
    )
    lines = receipt.read_text().splitlines()
    assert len(lines) == 2, f"expected exactly one two-line hand-off record; got: {lines!r}"
    return dict(line.split("=", 1) for line in lines)


def _run_self_update_via_seam(
    ocx: OcxRunner, repo: str, *extra_flags: str, home: Path
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx --format json [extra_flags] self update`` with the ``__OCX_SELF_IMAGE`` seam.

    ``home`` is keyword-only and mandatory, and must be a directory this test
    owns. The hand-off child runs a real ``ocx self setup``, which auto-detects
    its profile targets under ``$HOME`` (``.profile``, ``.bashrc``,
    ``.zprofile``, ``.zshrc``) — a default would eventually aim a test at the
    developer's own shell profiles.
    """
    home.mkdir(parents=True, exist_ok=True)
    env = dict(ocx.env)
    env["__OCX_SELF_IMAGE"] = f"{ocx.registry}/{repo}"
    env["HOME"] = str(home)
    return subprocess.run(
        [str(ocx.binary), "--format", "json", *extra_flags, "self", "update"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
        # `input=None` INHERITS fd 0. Same reason `OcxRunner.run` closes it:
        # a hand-off child reading the developer's terminal from a background
        # process group takes SIGTTIN and suspends the whole run.
        stdin=subprocess.DEVNULL,
    )


def _curate_local_index_to_v1(custom_index: Path) -> None:
    """Drop the ``0.0.2`` tag from the local index so it blesses only ``0.0.1``.

    Diverges the local index from the registry (which still holds both tags) so
    a test can prove whether a code path consults the local index or the
    registry for tag discovery.

    Selects the root document by *shape*, not by position. This used to take
    ``next(custom_index.rglob("*.json"))`` — "the first JSON file under the
    index home" — which is not a defined thing: the home also holds
    ``c/index.json``, and directory iteration order is not a contract. It
    picked a root by luck until the catalog gained its ``format_version``
    envelope, then picked the catalog and died on ``KeyError: 'tags'``. The
    exact-one assertion is the point: a missing root fails loudly here instead
    of curating nothing and leaving the caller to conclude the local index was
    consulted when it never was.
    """
    roots = [
        path
        for path in sorted(custom_index.rglob("*.json"))
        if "tags" in json.loads(path.read_text())
    ]
    assert len(roots) == 1, f"expected exactly one root document under {custom_index}, found {roots}"
    idx_file = roots[0]
    data = json.loads(idx_file.read_text())
    data["tags"].pop("0.0.2", None)
    idx_file.write_text(json.dumps(data))


@_skip_on_windows
def test_self_update_default_mode_queries_registry(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """Default-mode ``ocx self update`` discovers the latest version live from
    the registry, not the (possibly stale) local index.

    Regression for the "self update reads the local index" bug: self update
    exists to reach the freshest upstream release, so its version discovery uses
    ``TagProbe::Remote`` (matching the sibling ``ocx self setup`` bootstrap and
    the background auto-check that recommends running it), *not* the local index
    that a stale ``ocx index update`` snapshot would echo. With the local index
    blessing only ``0.0.1`` while the registry also holds ``0.0.2``, plain
    ``ocx self update`` (no ``--remote``) must find and install ``0.0.2``.

    Before the fix this reported ``up_to_date`` and forced the user onto the
    awkward ``ocx --remote self update``.
    """
    repo = unique_repo
    custom_index = tmp_path / "custom_index"
    custom_index.mkdir()
    ocx.env["OCX_INDEX"] = str(custom_index)

    _publish_two_versions(ocx, repo, tmp_path)  # indexes BOTH tags into custom_index
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")
    _curate_local_index_to_v1(custom_index)

    result = _run_self_update_via_seam(ocx, repo, home=tmp_path / "home")
    assert result.returncode == 0, (
        f"self update must exit 0; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert payload.get("status") == "installed" and payload.get("to") == "0.0.2", (
        f"default-mode self update must query the registry and install the registry-only 0.0.2, "
        f"not echo the curated local index (only 0.0.1 blessed); got: {payload!r}"
    )


@_skip_on_windows
def test_self_update_remote_flag_is_redundant(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """``ocx --remote self update`` behaves identically to the default: both
    query the registry live.

    Self update queries the registry by default now, so ``--remote`` is a no-op
    for it (still accepted for consistency with every other command). With the
    local index blessing only ``0.0.1`` but the registry holding ``0.0.2``,
    ``--remote`` must install ``0.0.2`` — same outcome as the default-mode test.
    """
    repo = unique_repo
    custom_index = tmp_path / "custom_index"
    custom_index.mkdir()
    ocx.env["OCX_INDEX"] = str(custom_index)

    _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")
    _curate_local_index_to_v1(custom_index)

    result = _run_self_update_via_seam(ocx, repo, "--remote", home=tmp_path / "home")
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
    """After ``self update``, the five ``$OCX_HOME/env.*`` shims exist — written
    by the CHILD, not by the parent.

    The parent enumerates no setup phases any more: ``setup::shims::refresh_shims``
    and the whole Decision 4C post-swap hook are deleted. Phase 2 of the spawned
    ``ocx self setup <tag>@<digest> --handoff`` writes the loaders, using the
    newly pulled version's own copy of their bodies.

    The hand-off receipt is asserted first and on purpose. Without it a green
    here says only "five files exist", which a parent that wrote them itself
    would satisfy just as well — the presence of the files is evidence about
    the machine, the receipt is evidence about *who* put them there.
    """
    repo = unique_repo
    receipt = _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")

    ocx_home = Path(ocx.env["OCX_HOME"])
    for shim in _ENV_SHIMS:
        assert not (ocx_home / shim).exists(), f"{shim} must not exist before the update"

    result = _run_self_update_via_seam(ocx, repo, home=tmp_path / "home")
    assert result.returncode == 0, (
        f"self update must exit 0; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert json.loads(result.stdout).get("status") == "installed", (
        f"self update must report status='installed'; got: {result.stdout!r}"
    )

    _hand_off_receipt(receipt)
    for shim in _ENV_SHIMS:
        assert (ocx_home / shim).is_file(), (
            f"the hand-off child's phase 2 must write {shim} after the binary swap"
        )


@_skip_on_windows
def test_self_update_heals_a_drifted_shim_and_hints_at_a_reload(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """A drifted ``env.sh`` is rewritten by the child, and the parent says so once.

    Pre-seed an ``$OCX_HOME/env.sh`` whose bytes differ from the canonical body.
    Phase 2 of the hand-off child is diff-gated, so it must notice the drift and
    rewrite the file with the newly pulled version's own shim.

    Two facts are asserted, and each rules out what the other cannot:

    * the seeded marker is **gone** and the canonical ``Managed by ocx installer``
      header is there instead — the child reached phase 2 and healed the file;
    * stderr carries the parent's ``Reload`` advisory verbatim. That line is
      reachable only from ``Installed { handoff: None }`` (``advisory_for`` in
      ``self_group/update.rs``), so it separates a completed hand-off from every
      failure wording. Non-interactively the advisory routes to ``log::info!``,
      hence the explicit ``--log-level info``.

    This test used to assert the bare substring ``"ocx self setup"`` on stderr.
    Do not put that back: the ``Pulled`` advisory ("the new ocx was downloaded
    but not activated (...); run 'ocx self setup'") and the child's own
    conflicting-ocx warning both contain it, so the assertion passed on a run
    that did the opposite of what this test is named for, and on a host with an
    ambient ``ocx`` on ``PATH`` it passed without any drift at all.
    """
    repo = unique_repo
    _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")

    ocx_home = Path(ocx.env["OCX_HOME"])
    ocx_home.mkdir(parents=True, exist_ok=True)
    drift_marker = "# stale shim content that will be rewritten"
    (ocx_home / "env.sh").write_text(f"{drift_marker}\n")

    result = _run_self_update_via_seam(
        ocx, repo, "--log-level", "info", home=tmp_path / "home"
    )
    assert result.returncode == 0, (
        f"self update must exit 0; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert json.loads(result.stdout).get("status") == "installed", (
        f"self update must report status='installed'; got: {result.stdout!r}"
    )

    healed = (ocx_home / "env.sh").read_text()
    assert drift_marker not in healed, (
        f"the child's diff-gated phase 2 must rewrite a drifted env.sh; it still reads:\n{healed}"
    )
    assert _CANONICAL_SHIM_HEADER in healed, (
        f"the healed env.sh must carry ocx's own shim body; got:\n{healed}"
    )
    assert _RELOAD_ADVISORY in result.stderr, (
        f"a completed hand-off must emit the Reload advisory ({_RELOAD_ADVISORY!r}) on stderr; "
        f"got:\n{result.stderr}"
    )


@_skip_on_windows
def test_self_update_does_not_touch_user_rc(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """``self update`` never introduces a managed block into a user shell profile.

    ``--handoff`` is passed unconditionally and threads to ``apply_target``'s
    ``heal_only``: the child rewrites a drifted block and migrates a legacy
    footprint, but must never *put a block into a profile that has none*. An
    update is not the moment to start managing someone's shell.

    The profile is seeded at ``$HOME/.profile`` — one of the targets
    ``detect_targets`` actually scans — and carries no ocx footprint, so
    "byte-identical" is the whole of the contract here and nothing about the
    legacy-migration branch is in scope. It used to be written to a path
    outside ``$HOME``, where the child could not have touched it whatever
    ``heal_only`` said, and the assertion held for that reason alone.
    """
    repo = unique_repo
    _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")

    home = tmp_path / "home"
    home.mkdir(parents=True, exist_ok=True)
    profile = home / ".profile"
    original = "# user content\nexport KEEP=1\n"
    profile.write_text(original)

    result = _run_self_update_via_seam(ocx, repo, home=home)
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
    """End-to-end: ``ocx self update`` upgrades by running the NEW binary's own
    setup, and the machine ends up with every surface that setup owns.

    This is the regression the hand-off exists for. Updating 0.6.0 -> 0.6.1
    left machines needing a manual ``ocx self setup``: the swap ran a hardcoded
    two-phase subset of ``setup::run`` **inside the old binary**, so an update
    to version N always applied version N-1's setup contract — and 0.6.1's new
    surface, the session-PATH stores, was one the subset had never heard of.

    Four things are asserted, and the last two are what make the first two
    mean something:

    1. the JSON wire shape (``installed`` / ``0.0.1`` -> ``0.0.2``) and exit 0;
    2. ``current`` names the 0.0.2 package, with no ``candidates/0.0.2``
       (self-update sets ``candidate=false``);
    3. the **hand-off receipt** — a surface only the child can write, so it
       proves the parent re-executed the newly pulled binary rather than
       finishing the job itself; and its ``current=`` line, which records the
       package ``current`` named *at the moment the child started*, still the
       0.0.1 one. That is the half a bare "the store is there" assertion
       cannot see: the select belongs to the child's first phase, and a
       regression to ``select = true`` in the parent leaves every other
       observable in this test unchanged;
    4. the session-PATH store, written only by a full ``setup::run`` — the
       surface whose absence was the original bug report.
    """
    repo = unique_repo
    receipt = _publish_two_versions(ocx, repo, tmp_path)

    # Pre-install 0.0.1 with --select so `current` points at it and
    # `query_installed_version` can read the running version back out.
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")
    current_symlink = _current_symlink(ocx, repo)
    assert_symlink_exists(current_symlink)
    before = current_symlink.resolve()

    home = tmp_path / "home"
    result = _run_self_update_via_seam(ocx, repo, home=home)

    assert result.returncode == 0, (
        f"`ocx self update` must exit 0 when a newer version is installed; "
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )

    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as e:
        raise AssertionError(
            f"`ocx --format json self update` must produce one JSON document on stdout; "
            f"got stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        ) from e
    assert payload.get("status") == "installed", (
        f"status must be 'installed' once `current` names the new package; got: {payload!r}"
    )
    assert payload.get("from") == "0.0.1", (
        f"JSON payload must report from='0.0.1'; got: {payload!r}\n"
        "If this is None, query_installed_version returned None — the 0.0.1 stand-in "
        "`bin/ocx` did not answer `--format json version`."
    )
    assert payload.get("to") == "0.0.2", f"JSON payload must report to='0.0.2'; got: {payload!r}"
    assert "handoff" not in payload, (
        f"a clean hand-off must report no failure detail; got: {payload!r}"
    )

    # `current` now resolves to the 0.0.2 package root (the post-install
    # symlink targets the root, not the content tree).
    resolved = current_symlink.resolve()
    assert resolved != before, (
        f"`current` must move off the 0.0.1 package; it still resolves to {before}"
    )
    probe = subprocess.run(
        [str(resolved / "content" / "bin" / "ocx"), "--format", "json", "version"],
        capture_output=True,
        text=True,
        env={"PATH": "/usr/bin:/bin"}, check=False,
    )
    assert probe.returncode == 0, (
        f"the binary `current` resolves to must run; rc={probe.returncode}, stderr={probe.stderr!r}"
    )
    assert json.loads(probe.stdout).get("version") == "0.0.2", (
        f"`current` must resolve to the 0.0.2 package; its binary reported {probe.stdout!r}"
    )

    # candidate=false: self-update never creates a candidate symlink.
    assert_not_exists(
        Path(ocx.env["OCX_HOME"]) / "symlinks" / registry_dir(ocx.registry) / repo / "candidates" / "0.0.2"
    )

    # The child ran, and it ran BEFORE the select.
    #
    # The digest is the tag's PLATFORM manifest, read back from the registry
    # rather than from anything this update wrote: the hand-off pins the exact
    # artifact the pull staged for this host, not the image index the tag names
    # (which is rewritten on every platform push).
    record = _hand_off_receipt(receipt)
    pinned = fetch_platform_manifest_digest(ocx.registry, repo, "0.0.2", platform=current_platform())
    assert record["argv"].split() == ["self", "setup", f"0.0.2@{pinned}", "--handoff"], (
        "the hand-off must invoke the new binary as `self setup <tag>@<digest> --handoff`, "
        f"pinned to the artifact just pulled ({pinned}); got: {record['argv']!r}"
    )
    assert record["current"] == str(before), (
        "the select belongs to the child's first phase: `current` must still name the OLD "
        f"package when the child starts. Expected {before}, the child saw {record['current']!r} — "
        "which is what a regression to `select = true` in the parent looks like"
    )

    # The surface the pre-hand-off subset never wrote.
    store = _session_path_store({**ocx.env, "HOME": str(home)})
    assert store is not None, f"no session-PATH store is modelled for {sys.platform!r}"
    assert store.is_file(), (
        f"the new binary's own setup must reach phase 3.5 and write {store}; "
        "an update that leaves it absent is the 0.6.0 -> 0.6.1 regression"
    )


@_skip_on_windows
def test_self_update_reports_pulled_and_exits_75_when_the_handoff_fails(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """A hand-off that never reaches its select leaves the machine unchanged:
    ``pulled``, exit 75, ``current`` where it was.

    The verdict is read off the ``current`` symlink, not off the child's exit
    status — so this is the case where both agree, and its value is the
    ``Pulled`` half of that contract: the release was downloaded, nothing was
    activated, and re-running is meaningful (``EX_TEMPFAIL``).
    """
    repo = unique_repo
    receipt = _publish_two_versions(ocx, repo, tmp_path, hand_off="refuse")
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")

    current_symlink = _current_symlink(ocx, repo)
    before = current_symlink.resolve()

    result = _run_self_update_via_seam(ocx, repo, home=tmp_path / "home")

    assert result.returncode == 75, (
        f"a pulled-but-not-activated update must exit 75 (EX_TEMPFAIL); rc={result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert payload.get("status") == "pulled", (
        f"status must be 'pulled' when `current` never moved; got: {payload!r}"
    )
    assert (payload.get("from"), payload.get("to")) == ("0.0.1", "0.0.2"), (
        f"a pulled outcome still reports both versions; got: {payload!r}"
    )
    assert payload.get("handoff") == {"reason": "exited", "detail": _HANDOFF_REFUSAL_CODE}, (
        f"the child's exit must be carried verbatim; got: {payload!r}"
    )

    # The child did start — this is a refusal, not a spawn failure.
    _hand_off_receipt(receipt)

    assert current_symlink.resolve() == before, (
        "a hand-off that never selected must leave `current` byte-for-byte where it was; "
        f"it now resolves to {current_symlink.resolve()}"
    )
    assert "downloaded but not activated" in result.stderr, (
        f"the advisory must say nothing was activated; got:\n{result.stderr}"
    )


@_skip_on_windows
def test_self_update_reports_installed_when_the_child_exits_82(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """A child that selects and *then* exits 82 is a successful update.

    Exit 82 (``DirtyRcBlock``) happens in the child's profile phase, which runs
    after its select — so the binary the user asked for is already the one
    ``current`` names. This is the case that separates a verdict keyed on the
    ``current`` symlink from one keyed on the child's exit status: the latter
    reports a completed update as a failure.

    The dirty block is real, not simulated: an isolated ``$HOME`` carries a
    ``.bashrc`` whose managed fence has a user edit inside it, which is exactly
    what the child's own ``rc_block`` state machine refuses to overwrite.
    """
    repo = unique_repo
    receipt = _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")

    current_symlink = _current_symlink(ocx, repo)
    before = current_symlink.resolve()

    home = tmp_path / "home"
    home.mkdir()
    bashrc = home / ".bashrc"
    tampered = f"# >>> ocx v1 {_canonical_hash(_FENCE_BODY)} >>>\n{_FENCE_BODY}\necho tampered\n# <<< ocx <<<\n"
    bashrc.write_text(tampered)

    result = _run_self_update_via_seam(ocx, repo, home=home)

    assert result.returncode == 0, (
        f"a swap that completed must exit 0 even when the child's setup ended 82; "
        f"rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    payload = json.loads(result.stdout)
    assert payload.get("status") == "installed", (
        f"the swap landed, so the status is 'installed' whatever the child exited with; got: {payload!r}"
    )
    assert payload.get("handoff") == {"reason": "exited", "detail": 82}, (
        f"the completed update must still carry the child's 82; got: {payload!r}"
    )

    _hand_off_receipt(receipt)
    assert current_symlink.resolve() != before, (
        "the child performed its select before the profile phase failed, so `current` must "
        f"have moved off {before}"
    )
    assert "ocx self setup --force" in result.stderr, (
        f"a dirty profile must be advised with the flag that overrides it; got:\n{result.stderr}"
    )
    assert bashrc.read_text() == tampered, (
        "the dirty block must be left byte-identical — that is why the child exited 82"
    )


@_skip_on_windows
def test_self_update_handoff_introduces_no_block_on_a_never_setup_machine(
    ocx: OcxRunner,
    tmp_path: Path,
    unique_repo: str,
) -> None:
    """Decision 4C: an update heals blocks, it never introduces one.

    A machine with ocx installed but never set up carries no managed fence in
    any profile. The hand-off child runs a full ``ocx self setup`` — with
    ``--handoff``, which puts its profile phase in heal-only mode — so every
    profile must come out byte-identical and no ocx fence may appear anywhere
    under ``$HOME``. Otherwise ``self update`` would silently adopt a surface
    the user opted out of.

    Guarded against passing vacuously: the receipt proves the child ran, so a
    green here cannot mean the profile phase was simply never reached.
    """
    repo = unique_repo
    receipt = _publish_two_versions(ocx, repo, tmp_path)
    ocx.json("package", "install", "-s", f"{repo}:0.0.1")

    home = tmp_path / "home"
    home.mkdir()
    # Every POSIX target `profiles::detect_targets` names, pre-seeded so the
    # assertion is about content rather than about a file that never existed.
    originals = {
        name: f"# {name} owned by the user\nexport KEEP_{index}=1\n"
        for index, name in enumerate((".profile", ".bashrc", ".zprofile", ".zshrc"))
    }
    for name, content in originals.items():
        (home / name).write_text(content)

    result = _run_self_update_via_seam(ocx, repo, home=home)

    assert result.returncode == 0, (
        f"self update must exit 0; rc={result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert json.loads(result.stdout).get("status") == "installed", (
        f"the swap must have completed, or the profile phase was never reached; got: {result.stdout!r}"
    )
    _hand_off_receipt(receipt)

    for name, content in originals.items():
        assert (home / name).read_text() == content, (
            f"a hand-off must not introduce a managed block into {name}; got:\n{(home / name).read_text()}"
        )
    fenced = sorted(path.name for path in home.iterdir() if path.is_file() and "# >>> ocx" in path.read_text())
    assert fenced == [], f"no profile may gain an ocx fence on a never-set-up machine; got: {fenced}"
