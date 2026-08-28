"""Acceptance tests for ``--frozen`` (issue #155): freeze tag resolution to the
local index.

``--frozen`` lets a tag already in the local index (and any digest-pinned
reference) resolve, but refuses to fetch + commit an unknown (un-indexed) tag —
that errors with exit 81 (``PolicyBlocked``) so CI can ``case $?`` on it.
Distinct from ``--offline``, which forbids all network: frozen still pulls
pinned digests over the network.

Modeled on ``test_offline.py`` / ``test_pinned_offline.py``.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src import OcxRunner
from src.helpers import make_package
from src.registry import fetch_manifest_digest, push_raw_config_package
from src.runner import PackageInfo, registry_dir


def _write_ocx_toml(project: Path, body: str) -> Path:
    path = project / "ocx.toml"
    path.write_text(body)
    return path


def _run_in_project(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ocx with the given args from ``cwd``, no exit check."""
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
    )


# Exit codes (stable; see crates/ocx_lib/src/cli/exit_code.rs).
POLICY_BLOCKED = 81
USAGE_ERROR = 64


def _run(
    ocx: OcxRunner, *args: str, extra_env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    """Run ocx with the instance env (plus optional overrides), no exit check."""
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [str(ocx.binary), *args], capture_output=True, text=True, env=env
    )


def _index_root(ocx: OcxRunner) -> Path:
    """The local index collection home: ``$OCX_HOME/index``.

    Holds every source's copy of the index wire grammar — per-package root
    documents (``<source>/p/<pkg>.json``) plus the dispatch-object CAS
    (``o/sha256/<hex>``) — under one root. Wiping it simulates a fresh machine
    with nothing locally indexed, so a tag can no longer resolve from the local
    index.
    """
    return Path(ocx.env["OCX_HOME"]) / "index"


def test_frozen_known_tag_resolves_from_local_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A fully-cached tag resolves under ``--frozen`` without walking the source.

    An online install caches the full manifest chain (tag pointer + manifest
    blobs) into the local store — the state a frozen resolve needs. The frozen
    re-resolve then hits that cache and never walks the source chain.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    # Online install caches the full chain (tag pointer + blobs) locally.
    ocx.json("package", "install", pkg.short)

    result = ocx.run("--frozen", "package", "install", "--select", pkg.short, check=False)
    assert result.returncode == 0, (
        f"--frozen install of a fully-cached tag must succeed; rc={result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_frozen_unknown_tag_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """An unpinned tag missing from the local index errors with exit 81.

    The tag exists on the registry, but ``--frozen`` refuses to walk the source
    chain to fetch + commit an un-indexed reference — the deliberate policy.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    # Drop the local index so the tag is no longer locally known.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    result = _run(ocx, "--frozen", "package", "install", pkg.short)
    assert result.returncode == POLICY_BLOCKED, (
        f"--frozen install of an un-indexed tag must exit 81 (PolicyBlocked); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_digest_pinned_succeeds(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A digest-pinned reference still fetches content under ``--frozen``.

    Wiping the local index first proves frozen fetched the pinned content from
    the registry rather than relying on a cached tag pointer — the digest axis
    is what distinguishes frozen from offline.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    pinned = f"{pkg.fq}@{digest}"

    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    result = _run(ocx, "--frozen", "package", "install", pinned)
    assert result.returncode == 0, (
        f"--frozen install of a digest-pinned ref must succeed; "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_with_remote_flag_exits_64(ocx: OcxRunner) -> None:
    """``--frozen --remote`` is a contradiction → clap usage error (exit 64)."""
    result = _run(ocx, "--frozen", "--remote", "package", "install", "whatever:1")
    assert result.returncode == USAGE_ERROR, (
        f"--frozen --remote must be a usage error (exit 64); rc={result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_frozen_bare_repo_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A bare repo reference (no tag) under ``--frozen`` normalises to ``latest``
    and exits 81 when that tag is not in the local index.

    Bare identifiers normalise to ``:latest`` inside ``Identifier::tag_or_latest()``.
    The resulting unpinned tag is absent from the local index (the package was
    pushed but never installed, so no tag pointer was committed).  The frozen
    policy gate fires on the tag-only path (``identifier.digest().is_none()``),
    not on ``latest`` specifically — so this test also exercises the bare-
    identifier normalisation branch of ``ChainedIndex::walk_chain``.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)

    # Wipe the local index so ``latest`` is not indexed.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    # Bare identifier: no tag component — normalises to ``:latest`` internally.
    bare = f"{ocx.registry}/{unique_repo}"
    result = _run(ocx, "--frozen", "package", "install", bare)
    assert result.returncode == POLICY_BLOCKED, (
        f"--frozen install of a bare (unindexed) repo must exit 81 (PolicyBlocked); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_lock_blocks_unindexed_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--frozen lock`` exits 81 when an ``ocx.toml`` tool references a tag
    that is not in the local index.

    Exercises the project-tier resolve path (``project/resolve.rs`` →
    ``retry_fetch`` → ``policy_block_label`` → ``ProjectErrorKind::PolicyBlocked``)
    which has unit coverage but was previously untested end-to-end via
    acceptance tests.

    The package exists on the registry but was never installed (no tag pointer
    committed to the local index), so ``--frozen lock`` must refuse to walk the
    source chain and exit 81.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    # Ensure the local index is empty so the tag pointer is absent.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f'[tools]\n{unique_repo} = "{pkg.fq}"\n',
    )

    result = _run_in_project(ocx, project, "--frozen", "lock")
    assert result.returncode == POLICY_BLOCKED, (
        f"--frozen lock with an unindexed tag must exit 81 (PolicyBlocked); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_allows_direct_registry_query(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--frozen`` does not block commands that query the registry directly.

    ``ocx package info`` calls ``Publisher::pull_description`` via
    ``context.remote_client()`` — the raw OCI client — bypassing the Index
    facade entirely.  The frozen policy only gates tag resolution through
    ``ChainedIndex``; it has no effect on code paths that never call
    ``default_index()``.

    Setup: push a package so the repo exists in the registry, then wipe the
    local index so the tag is absent.  ``--frozen package info`` must still
    exit 0 because it does not perform tag resolution through the index.

    Routing evidence:
      ``crates/ocx_cli/src/command/package_info.rs``:35 —
        ``Publisher::new(context.remote_client()?.clone())``
      ``crates/ocx_cli/src/app/context.rs``:249 —
        ``remote_client()`` only errors on ``OfflineMode``, not ``FrozenMode``
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    # Wipe the local index so the tag is absent — frozen would block install.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    # ``package info`` queries the ``__ocx.desc`` tag via remote_client(),
    # not the Index.  No description has been pushed, so it returns empty
    # results — but the command exits 0, not 81.
    result = _run(ocx, "--frozen", "package", "info", pkg.short)
    assert result.returncode == 0, (
        f"--frozen package info must succeed (direct registry query bypasses index "
        f"resolution); rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_add_blocks_unindexed_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--frozen add <repo:tag>`` exits 81 when the tag is not in the local index.

    ``ocx add`` resolves the tag through ``context.default_index()``, which
    carries ``ChainMode::Frozen`` when ``--frozen`` is set.  The frozen chain
    refuses to walk the source chain for an unpinned tag absent from the local
    index, routing through ``project/resolve.rs`` →
    ``policy_block_label`` → ``ProjectErrorKind::PolicyBlocked`` → exit 81.

    This mirrors ``test_frozen_lock_blocks_unindexed_tag`` for the ``add``
    command, confirming that the policy gate fires on every project-tier
    command that calls ``resolve_lock`` rather than only on ``lock``.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    # Ensure the local index is empty so the tag pointer is absent.
    index_home = _index_root(ocx)
    if index_home.exists():
        shutil.rmtree(index_home)

    project = tmp_path / "proj_add"
    project.mkdir()
    _write_ocx_toml(project, "")  # minimal valid ocx.toml (no tools yet)

    result = _run_in_project(ocx, project, "--frozen", "add", pkg.fq)
    assert result.returncode == POLICY_BLOCKED, (
        f"--frozen add with an unindexed tag must exit 81 (PolicyBlocked); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


def test_frozen_remote_env_conflict_exits_64(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``OCX_FROZEN=1`` + ``OCX_REMOTE=1`` (env, no flags) → runtime check, exit 64.

    clap's ``conflicts_with`` cannot see env-sourced defaults; the runtime
    ``check_frozen_remote_exclusivity`` guard closes that gap.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    result = _run(
        ocx,
        "package",
        "install",
        pkg.short,
        extra_env={"OCX_FROZEN": "1", "OCX_REMOTE": "1"},
    )
    assert result.returncode == USAGE_ERROR, (
        f"OCX_FROZEN + OCX_REMOTE must hit the runtime exclusivity check (exit 64); "
        f"rc={result.returncode}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# `--frozen` and the patch tier (issue #293)
#
# `--frozen` scopes to the PACKAGE tier. Patches float by design, so a
# companion resolves live even under the flag — its binding is patch-tier
# state (`state/patch-companions/`), never a local-index pin. The helpers below
# are deliberately local copies of the `test_patches.py` ones so this module
# reads on its own.
# ---------------------------------------------------------------------------


def _unique_repo(label: str) -> str:
    """Generate a unique OCI repository name for within-test use."""
    return f"t_{uuid4().hex[:8]}_{label}"


def _write_patches_config(ocx: OcxRunner, patch_registry: str, *, required: bool) -> None:
    """Write `$OCX_HOME/config.toml` with a `[patches]` tier."""
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        f"[patches]\nregistry = \"{patch_registry}\"\nrequired = {str(required).lower()}\n"
    )


def _make_companion(
    ocx: OcxRunner, repo: str, tag: str, tmp_path: Path, key: str, value: str, *, index: bool = True
) -> PackageInfo:
    """Publish a binary-free companion exposing one INTERFACE env var.

    Published `platform="any"` for the same reason `test_patches.py` does: an
    env-only companion has no host-specific content, and `ocx patch sync` fans
    out over the full concrete ship matrix.
    """
    return make_package(
        ocx,
        repo,
        tag,
        tmp_path,
        bins=[],
        env=[{"key": key, "type": "constant", "value": value, "visibility": "interface"}],
        cascade=True,
        platform="any",
        index=index,
    )


def _write_descriptor(path: Path, rules: list[dict]) -> None:
    """Write a patch descriptor JSON file."""
    path.write_text(json.dumps({"version": 1, "rules": rules}))


def _publish_descriptor_at_base(ocx: OcxRunner, descriptor_path: Path, base_fq: str) -> None:
    """Publish a descriptor at the per-base path."""
    result = ocx.plain("patch", "publish", "--descriptor", str(descriptor_path), base_fq)
    assert result.returncode == 0, f"patch publish at base {base_fq} failed:\n{result.stderr}"


def _companion_pin_path(ocx: OcxRunner, registry: str, repo: str) -> Path:
    """The patch tier's own pin record for `repo` — never the local index."""
    return ocx.ocx_home / "state" / "patch-companions" / registry_dir(registry) / f"{repo}.json"


def _frozen_env_entries(ocx: OcxRunner, target: str, *root_args: str) -> list[dict]:
    """Return `entries` from `ocx --frozen [root_args...] package env <target>`."""
    result = _run(ocx, "--frozen", *root_args, "--format", "json", "package", "env", target)
    assert result.returncode == 0, (
        f"--frozen package env must succeed; rc={result.returncode}\nstderr: {result.stderr}"
    )
    return json.loads(result.stdout)["entries"]


def _entry_by_key(entries: list[dict], key: str) -> dict | None:
    """Return the first entry with the given key, or None."""
    return next((e for e in entries if e["key"] == key), None)


def test_frozen_index_update_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx index update` under `--frozen` refuses with exit 81.

    `index update` is the PACKAGE tier's discovery verb: its whole job is
    learning a new tag→digest binding and writing it into the local index.
    That is precisely what `--frozen` forbids, so it must refuse the way the
    other tiers' explicit update verbs already do (`ocx patch sync`,
    `ocx config update` — both exit 81), rather than quietly moving pins under
    a policy the user set to stop exactly that.

    The `[patches]` tier is configured so the piggyback sync is in scope too:
    the refusal happens before it, so nothing about it can surface either.
    """
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    _write_patches_config(ocx, registry, required=False)

    result = _run(ocx, "--frozen", "index", "update", base.short)
    assert result.returncode == POLICY_BLOCKED, (
        f"--frozen index update must exit 81 (PolicyBlocked); rc={result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert "without --frozen" in result.stderr, (
        "the refusal must name re-running without --frozen as the remedy; "
        f"stderr: {result.stderr}"
    )
    assert "patch descriptor sync failed" not in result.stderr, (
        "the refusal happens before the patch piggyback, so it cannot warn about it; "
        f"stderr: {result.stderr}"
    )


@pytest.mark.xdist_group("patch_global_slot")
def test_frozen_install_composes_a_pinned_companion(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A companion that IS pinned still installs and composes under `--frozen`.

    The over-block guard: freezing the patch tier must not break the case the
    freeze exists to serve. The pin is digest-addressed, so pulling it is
    content fetching, not discovery.
    """
    companion_repo = _unique_repo("frozen_pinned")
    companion = _make_companion(
        ocx, companion_repo, "1.0.0", tmp_path / "c", "FROZEN_PINNED_CA", "/certs/pinned/ca.pem"
    )
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor = tmp_path / "pinned_descriptor.json"
    _write_descriptor(descriptor, rules=[{"match": "*", "packages": [companion.fq]}])
    _write_patches_config(ocx, registry, required=False)
    _publish_descriptor_at_base(ocx, descriptor, base.fq)

    # Online install records the descriptor and pins the companion.
    ocx.plain("package", "install", base.short)
    assert _companion_pin_path(ocx, registry, companion_repo).exists(), (
        "setup: the online install must have pinned the companion in patch state"
    )

    frozen_install = _run(ocx, "--frozen", "package", "install", base.short)
    assert frozen_install.returncode == 0, (
        f"--frozen install with a pinned companion must succeed; rc={frozen_install.returncode}\n"
        f"stderr: {frozen_install.stderr}"
    )

    entry = _entry_by_key(_frozen_env_entries(ocx, base.short), "FROZEN_PINNED_CA")
    assert entry is not None, "a pinned companion must still compose under --frozen"
    assert entry["value"] == "/certs/pinned/ca.pem"


@pytest.mark.xdist_group("patch_global_slot")
def test_snapshot_install_records_the_pin_it_adopted(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A snapshot-driven companion install fills the patch-tier record in.

    A machine that only ever installs under `OCX_PATCH_SNAPSHOT` — a cold CI
    runner with a committed `patches.snapshot.json` — resolves every companion
    from the snapshot, so nothing writes the record. But the record is what
    BOTH `ocx patch freeze` and record-scoped GC read: a freeze there would
    replace the good snapshot with an empty companion map, and `ocx clean`
    would collect the very package the next snapshot build composes.

    The install records the pin it adopted, so the machine's live state matches
    what is actually installed. Deleting the record reproduces the cold runner.
    The recorded digest is the snapshot's — the platform manifest, where a
    discovery-written record holds the image index above it — so the assertion
    compares against the snapshot, and the decisive check is that a re-freeze
    reproduces that same snapshot byte for byte.
    """
    companion_repo = _unique_repo("snapshot_record")
    companion = _make_companion(
        ocx, companion_repo, "1.0.0", tmp_path / "c", "RECORDED_CA", "/certs/recorded/ca.pem"
    )
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor = tmp_path / "record_descriptor.json"
    _write_descriptor(descriptor, rules=[{"match": "*", "packages": [companion.fq]}])
    _write_patches_config(ocx, registry, required=False)
    _publish_descriptor_at_base(ocx, descriptor, base.fq)
    ocx.plain("package", "install", base.short)

    freeze = ocx.run("--global", "patch", "freeze", format="json", check=False)
    assert freeze.returncode == 0, f"setup: patch freeze must succeed:\n{freeze.stderr}"
    snapshot_path = ocx.ocx_home / "patches.snapshot.json"
    snapshot = json.loads(snapshot_path.read_text())
    # The companions map is keyed by the tag-bearing identifier — one
    # repository can be named at two tags, and each is its own companion.
    pinned_digest = snapshot["companions"].get(companion.fq)
    assert pinned_digest is not None, (
        f"setup: the freeze must have pinned the companion; got {snapshot}"
    )

    # The cold runner: a snapshot, and no patch-tier record at all.
    pin_path = _companion_pin_path(ocx, registry, companion_repo)
    pin_path.unlink()

    install = _run(
        ocx,
        "--global",
        "package",
        "install",
        base.short,
        extra_env={"OCX_PATCH_SNAPSHOT": str(snapshot_path)},
    )
    assert install.returncode == 0, (
        f"a snapshot-pinned companion must install; rc={install.returncode}\n"
        f"stderr: {install.stderr}"
    )
    assert pin_path.exists(), (
        "the pin adopted from the snapshot must be recorded, or freeze and GC cannot see it"
    )
    assert json.loads(pin_path.read_text()) == {companion.tag: pinned_digest}, (
        f"the recorded pin must name the digest the snapshot pinned; got {pin_path.read_text()}"
    )

    refreeze = ocx.run("--global", "patch", "freeze", format="json", check=False)
    assert refreeze.returncode == 0, f"patch freeze must succeed:\n{refreeze.stderr}"
    assert json.loads(snapshot_path.read_text()) == snapshot, (
        "a re-freeze on a machine that only ever installed from the snapshot must reproduce "
        f"the same snapshot, not overwrite it with an empty companion map; got "
        f"{snapshot_path.read_text()}"
    )


def _assert_no_index_footprint(ocx: OcxRunner, registry: str, repo: str, why: str) -> None:
    """Assert ``repo`` owns **zero bytes** anywhere under ``$OCX_HOME/index``.

    The root document is only the visible half of the package tier. A companion
    is pulled ``tag@digest``, which commits no tag pointer but did keep writing
    the DISPATCH OBJECT into ``p/<repo>/o/<algo>/<hex>.json`` — the same shared
    index a `--frozen` run is asserting it does not touch. Asserting on the
    root document alone passes straight over it.
    """
    package_dir = _index_root(ocx) / registry_dir(registry) / "p" / repo
    root_document = package_dir.with_name(f"{package_dir.name}.json")
    leftovers = sorted(str(p.relative_to(ocx.ocx_home)) for p in package_dir.rglob("*") if p.is_file())
    assert not root_document.exists() and not leftovers, (
        f"{why}: {repo} must own nothing under the local index; found "
        f"{'the root document ' if root_document.exists() else ''}{leftovers}"
    )


@pytest.mark.xdist_group("patch_global_slot")
def test_frozen_config_setup_installs_floating_patch_companions(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """`ocx --frozen config setup` delivers the payload's patch companions.

    Issue #293: `--frozen` scopes to the PACKAGE tier. Managed config and the
    patches it distributes are not that tier, so a frozen setup must behave
    exactly like an unfrozen one — fetch the payload, and let the piggyback
    sync install the companions the payload's `[patches]` registry names.

    The companion is published with `index=False` and named by an unpinned tag,
    so the local index cannot answer for it: only a live, mode-independent
    resolve reaches it. Its binding lands in the patch tier's own pin record —
    never as a local-index root document, which stays the package tier's.
    """
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    ocx.plain("package", "install", base.short)

    companion_repo = _unique_repo("frozen_managed_companion")
    companion = _make_companion(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path / "c",
        "FROZEN_MANAGED_CA",
        "/certs/managed/ca.pem",
        index=False,
    )

    descriptor = tmp_path / "managed_descriptor.json"
    _write_descriptor(descriptor, rules=[{"match": "*", "packages": [companion.fq]}])
    _write_patches_config(ocx, registry, required=False)
    _publish_descriptor_at_base(ocx, descriptor, base.fq)
    # The patch tier must reach this run through the managed payload alone.
    (Path(ocx.env["OCX_HOME"]) / "config.toml").unlink()

    config_repo = f"{unique_repo}_cfg"
    ref = f"{registry}/{config_repo}:v1"
    push_raw_config_package(
        registry,
        config_repo,
        "v1",
        f'[patches]\nregistry = "{registry}"\nrequired = false\n'.encode(),
    )

    setup = _run(ocx, "--format", "json", "--frozen", "config", "setup", "--managed-config", ref)
    assert setup.returncode == 0, (
        f"--frozen config setup must work like an unfrozen one; rc={setup.returncode}\n"
        f"stdout: {setup.stdout}\nstderr: {setup.stderr}"
    )

    pin_path = _companion_pin_path(ocx, registry, companion_repo)
    assert pin_path.exists(), (
        "the piggyback sync must have installed and pinned the payload's companion; "
        f"expected a pin at {pin_path}\nstderr: {setup.stderr}"
    )
    entry = _entry_by_key(_frozen_env_entries(ocx, base.short), "FROZEN_MANAGED_CA")
    assert entry is not None, "the live-resolved companion must compose under --frozen"
    assert entry["value"] == "/certs/managed/ca.pem"

    # Last, so the check spans BOTH producers: the install's pinned pull and
    # the compose above, whose blob-store read of the same image index would
    # otherwise self-heal it back into the index.
    _assert_no_index_footprint(
        ocx,
        registry,
        companion_repo,
        "a companion is named by a descriptor, not by the user, so nothing about it enters the "
        "package tier — not a root document, not a dispatch object",
    )


@pytest.mark.xdist_group("patch_global_slot")
def test_frozen_install_resolves_an_unpinned_companion_live(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """A frozen install of a patched base resolves its companion live.

    Patches float by design, so companion resolution never goes through the
    frozen ambient chain. The companion is published `index=False` and named by
    tag, so the local index holds nothing for it — only the live view can
    resolve it — and the pin it leaves behind is patch-tier state.
    """
    companion_repo = _unique_repo("frozen_live")
    companion = _make_companion(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path / "c",
        "FROZEN_LIVE_CA",
        "/certs/live/ca.pem",
        index=False,
    )
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=True)
    descriptor = tmp_path / "live_descriptor.json"
    _write_descriptor(
        descriptor, rules=[{"match": "*", "packages": [companion.fq], "required": True}]
    )
    _write_patches_config(ocx, registry, required=False)
    _publish_descriptor_at_base(ocx, descriptor, base.fq)

    result = _run(ocx, "--frozen", "package", "install", base.short)
    assert result.returncode == 0, (
        f"a frozen install must resolve an unpinned companion live; rc={result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert _companion_pin_path(ocx, registry, companion_repo).exists(), (
        "the live resolve must record the companion's pin in patch state"
    )
    entry = _entry_by_key(_frozen_env_entries(ocx, base.short), "FROZEN_LIVE_CA")
    assert entry is not None, "the live-resolved companion must compose under --frozen"
    assert entry["value"] == "/certs/live/ca.pem"

    # Last, so the check spans the install's pinned pull AND the compose above.
    _assert_no_index_footprint(
        ocx, registry, companion_repo, "resolving a companion must not grow the package tier's local index"
    )
