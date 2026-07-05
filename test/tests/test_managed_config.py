# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the corporate managed-configuration tier (``[managed]``).

Design record: ``.claude/artifacts/adr_managed_config_tier.md``;
``.claude/state/plans/plan_managed_config.md`` section "Acceptance criteria
(29)" is the single source of truth for the 29 numbered criteria referenced
below.

These tests run against the CURRENT STUBS (Phase 3, contract-first TDD, same
posture as ``test_oci_registry_mirror.py``): every scenario below is expected
to FAIL against the stubs today (``ocx config update``, ``ocx self setup
--managed-config``'s fetch/persist path, and the background-refresh probe are
all ``unimplemented!()``/``todo!()``). Assertions target the INTENDED FINAL
behavior, never the panic text.

Criterion -> coverage map (criterion -> test name, or "unit: <location>" when
fully covered at the Rust unit level instead):

| # | Coverage |
|---|---|
| 1 | ``test_fresh_home_no_seed_is_noop_parity`` |
| 2 | ``test_onboard_syncs_before_fence_write`` |
| 3 | ``test_resetup_same_ref_is_noop_no_refetch`` |
| 4 | ``test_resetup_different_ref_re_adopts`` |
| 5 | ``test_dirty_fence_exit_82_force_overwrites`` |
| 6 | ``test_required_no_snapshot_exit_78_online_and_offline`` |
| 7 | unit: ``config::managed::tests::resolve_managed_config_snapshot_source_mismatch_treated_as_absent`` + ``..._tag_vs_digest_mismatch_treated_as_absent``; ``config::loader::tests::managed_snapshot_source_mismatch_is_never_merged`` |
| 8 | unit: ``config::managed::tests::resolve_managed_config_required_false_absent_snapshot_returns_ok_some`` |
| 9 | ``test_env_only_override_never_writes_seed_and_reverts_without_env`` |
| 10 | unit: ``config::managed::tests::resolve_managed_config_empty_env_override_treated_as_unset`` |
| 11 | unit: ``config::loader::tests::managed_snapshot_merges_above_home_below_config_and_strips_managed_section`` |
| 12 | unit: ``config::patch.rs`` / ``config.rs`` system-lock precedent — ``[patches]`` beats managed payload via the same `Config::merge` fold (pre-existing, unchanged by this feature) |
| 13 | unit: ``config::managed::tests::resolve_managed_config_carries_system_required`` (system-scope `required=true` non-loosenable) |
| 14 | ``test_config_update_bypasses_throttle`` |
| 15 | unit: ``package_manager::tasks::managed_config::tests::check_managed_config_refresh_notify_on_drift_reports_without_persisting`` (+ ``..._notify_probe_does_not_pull_layer_blob``) — the notify tick is TTY-gated in the CLI, so it is covered at the unit level with a mocked ``Client`` transport (``test_transport.rs``). |
| 16 | unit: ``package_manager::tasks::managed_config::tests::check_managed_config_refresh_apply_on_drift_persists_silently`` — the apply drift-swap tick, covered at the unit level (same TTY-gate reason as #15). |
| 17 | ``test_config_update_registry_down_leaves_snapshot_untouched`` |
| 18 | unit: ``managed_config::persistence::tests::persist_managed_config_layer_digest_mismatch_leaves_snapshot_untouched`` + ``..._manifest_digest_mismatch_errors`` |
| 19 | ``test_ci_env_only_recipe_no_fence_required_satisfied`` |
| 20 | ``test_setup_fetch_failure_no_fence_no_partial_state`` |
| 21 | unit: ``managed_config::persistence::tests::persist_managed_config_concurrent_writers_leave_one_consistent_snapshot`` — concurrent double-apply race covered by a targeted Rust concurrency test (mirrors `StateStore`'s `touch_atomic_concurrent_safety` pattern). |
| 22 | not applicable to `[managed]` — this criterion is about `config.toml` reflow *around* the seed fence, already covered generically by `setup::rc_block` reflow tests (`rc_block.rs`'s existing CRLF/format-upgrade suite covers the mechanism; it is label-agnostic). |
| 23 | ``test_managed_fetch_honors_local_mirror_ignores_payload_mirror`` |
| 24 | unit: ``setup::rc_block::tests::managed_config_fence_body_is_toml_injection_safe`` |
| 25 | ``test_no_config_refresh_kill_switch_silences_debug_hook_but_not_explicit_update`` |
| 26 | ``test_no_config_hermetic_suppresses_candidate_and_env_override`` |
| 27 | ``test_zip_and_move_warm_home_offline_identical`` |
| 28 | unit: ``config::loader::tests::managed_snapshot_cannot_override_system_locked_registry`` — mirrors the sanctioned pattern in ``test_patches.py::test_launcher_digest_matched_opt_out_respects_system_required``: a SYSTEM-scope ``/etc/ocx/config.toml`` is the only thing that sets `system_locked`, which acceptance tests cannot write without root. |
| 29 | ``test_config_update_check_probe_reports_status_and_json_via_root_format`` |
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

from src.helpers import make_package, push_managed_config
from src.registry import push_raw_config_package
from src.runner import OcxRunner

# The managed-config fence's own label (ADR "Seed write mechanism" example:
# `# >>> ocx managed v1 <hash> >>>` ... `# <<< ocx managed <<<`) — distinct
# from the shell-activation fence's `ocx` label.
_MANAGED_FENCE_CLOSER = "# <<< ocx managed <<<"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def write_home_config(ocx: OcxRunner, content: str) -> Path:
    """Writes `content` to `$OCX_HOME/config.toml` (mirrors test_config.py)."""
    path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    path.write_text(content)
    return path


def _publish_self_image(ocx: OcxRunner, tmp_path: Path, repo: str) -> str:
    """Publishes a stand-in `ocx` package to the LOCAL test registry and
    returns the `__OCX_SELF_IMAGE` value that redirects the canonical
    `ocx.sh/ocx/cli` bootstrap identifier to it.

    `self setup`'s bootstrap phase always does a LIVE tag probe against the
    real `ocx.sh` registry for the unpinned path (`check_update` with
    `Duration::ZERO` + `TagProbe::Remote`), regardless of local state — a
    plain "seed a candidate file" trick cannot short-circuit that. The
    sanctioned test-only escape hatch is the `__OCX_SELF_IMAGE` loopback seam
    (see `test_self_setup.py::test_setup_bootstrap_pulls_latest_published`),
    compile-gated behind `--features ocx/__testing` (the test binary must be
    built with it) and loopback-only-asserted at runtime.
    """
    make_package(
        ocx,
        repo,
        "0.0.1",
        tmp_path,
        new=True,
        cascade=False,
        bins=["ocx"],
        outputs={"ocx": {"--format json version": json.dumps({"version": "0.0.1"})}},
    )
    return f"{ocx.registry}/{repo}"


def _run(
    ocx: OcxRunner,
    *args: str,
    env_overrides: dict[str, str] | None = None,
    drop_env: set[str] | None = None,
    format: str | None = "json",
    log_level: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Runs `ocx` with flexible env control (base env + drops + overrides)."""
    env = dict(ocx.env)
    for key in drop_env or ():
        env.pop(key, None)
    if env_overrides:
        env.update(env_overrides)
    cmd = [str(ocx.binary)]
    if format:
        cmd += ["--format", format]
    if log_level:
        cmd += ["--log-level", log_level]
    cmd += list(args)
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


def _self_setup(
    ocx: OcxRunner,
    ref: str,
    self_image: str,
    *extra_args: str,
    env_overrides: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Runs `ocx self setup --managed-config <ref> --no-modify-path [...]`.

    `self_image` is the `__OCX_SELF_IMAGE` seam value from
    [`_publish_self_image`] — required so bootstrap never reaches the real
    `ocx.sh` registry. Always passes `--no-modify-path` since these tests only
    care about the `[managed]` fence in `config.toml`, never the
    shell-profile RC blocks (a distinct fence, a distinct file, unrelated to
    `--managed-config`). Deliberately does NOT pass `--offline` — the
    managed-config sync fetch needs the real (local) test registry.
    """
    overrides = {"__OCX_SELF_IMAGE": self_image}
    if env_overrides:
        overrides.update(env_overrides)
    return _run(
        ocx, "self", "setup", "--managed-config", ref, "--no-modify-path", *extra_args, env_overrides=overrides
    )


def _registry_probe(ocx: OcxRunner, **kwargs: object) -> str:
    """Runs `ocx package install <bogus>` with `OCX_DEFAULT_REGISTRY` dropped
    and returns combined stdout+stderr, so the resolved registry default is
    observable in the failure message (mirrors test_config.py's strategy)."""
    result = _run(
        ocx,
        "package",
        "install",
        "nonexistent_pkg_ocx_test:0",
        drop_env={"OCX_DEFAULT_REGISTRY"},
        **kwargs,  # type: ignore[arg-type]
    )
    return result.stdout + result.stderr


# ---------------------------------------------------------------------------
# Criterion 1 — fresh HOME, no seed → no-op parity
# ---------------------------------------------------------------------------


def test_fresh_home_no_seed_is_noop_parity(ocx: OcxRunner) -> None:
    """A fresh `$OCX_HOME` with no `[managed]` seed behaves byte-identically
    to today: no managed-config state directory is ever created, and ordinary
    commands are unaffected."""
    result = _run(ocx, "index", "catalog")
    assert result.returncode == 0, f"index catalog must succeed on a fresh HOME: {result.stderr}"
    assert not (Path(ocx.env["OCX_HOME"]) / "state" / "managed-config").exists()


# ---------------------------------------------------------------------------
# Criterion 2 — onboard: sync fetch+persist first, fence on success
# ---------------------------------------------------------------------------


def test_onboard_syncs_before_fence_write(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "onboard-managed.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")

    result = _self_setup(ocx, ref, self_image)
    assert result.returncode == 0, f"onboard setup must succeed: {result.stderr}"

    config_text = (Path(ocx.env["OCX_HOME"]) / "config.toml").read_text()
    assert "[managed]" in config_text
    assert f'source = "{ref}"' in config_text

    snapshot_path = Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "snapshot.json"
    assert snapshot_path.exists(), "a synced snapshot must exist after a successful onboard"

    combined = _registry_probe(ocx)
    assert "onboard-managed.example" in combined, (
        f"the next command must merge the synced tier; got: {combined!r}"
    )


# ---------------------------------------------------------------------------
# Criterion 3 — same-ref re-setup: fence Current, no re-fetch
# ---------------------------------------------------------------------------


def test_resetup_same_ref_is_noop_no_refetch(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "resetup-same.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    snapshot_path = Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "snapshot.json"

    first = _self_setup(ocx, ref, self_image)
    assert first.returncode == 0, f"first setup must succeed: {first.stderr}"
    first_fetched_at = json.loads(snapshot_path.read_text())["fetched_at"]

    second = _self_setup(ocx, ref, self_image)
    assert second.returncode == 0, f"same-ref re-setup must succeed: {second.stderr}"
    second_fetched_at = json.loads(snapshot_path.read_text())["fetched_at"]

    assert second_fetched_at == first_fetched_at, "same-ref re-setup must not re-fetch (fence Current)"


# ---------------------------------------------------------------------------
# Criterion 4 — different-ref re-setup: re-adopt (fence rewrite + fresh fetch)
# ---------------------------------------------------------------------------


def test_resetup_different_ref_re_adopts(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    repo_a, repo_b = f"{unique_repo}_a", f"{unique_repo}_b"
    ref_a = f"{registry}/{repo_a}:v1"
    ref_b = f"{registry}/{repo_b}:v1"
    push_raw_config_package(registry, repo_a, "v1", b'[registry]\ndefault = "ref-a.example"\n')
    push_raw_config_package(registry, repo_b, "v1", b'[registry]\ndefault = "ref-b.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")

    assert _self_setup(ocx, ref_a, self_image).returncode == 0
    result = _self_setup(ocx, ref_b, self_image)
    assert result.returncode == 0, f"re-adopt to a different ref must succeed: {result.stderr}"

    config_text = (Path(ocx.env["OCX_HOME"]) / "config.toml").read_text()
    assert f'source = "{ref_b}"' in config_text
    assert ref_a not in config_text, "the old ref must not linger in the rewritten fence"

    combined = _registry_probe(ocx)
    assert "ref-b.example" in combined
    assert "ref-a.example" not in combined


# ---------------------------------------------------------------------------
# Criterion 5 — dirty fence → exit 82; --force overwrites
# ---------------------------------------------------------------------------


def test_dirty_fence_exit_82_force_overwrites(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "dirty-fence.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    assert _self_setup(ocx, ref, self_image).returncode == 0

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    text = config_path.read_text()
    assert _MANAGED_FENCE_CLOSER in text, f"expected the managed fence closer in config.toml, got: {text!r}"
    config_path.write_text(text.replace(_MANAGED_FENCE_CLOSER, f"# tampered by test\n{_MANAGED_FENCE_CLOSER}"))

    dirty = _self_setup(ocx, ref, self_image)
    assert dirty.returncode == 82, f"a dirty managed fence must exit 82, got {dirty.returncode}: {dirty.stderr}"

    forced = _self_setup(ocx, ref, self_image, "--force")
    assert forced.returncode == 0, f"--force must overwrite the dirty fence: {forced.stderr}"
    assert "tampered by test" not in config_path.read_text()


# ---------------------------------------------------------------------------
# Criterion 6 — required + no snapshot → exit 78, online AND --offline
# ---------------------------------------------------------------------------


def test_required_no_snapshot_exit_78_online_and_offline(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    # Never adopted (no `self setup` run) -> no snapshot exists at all, and
    # `required` defaults to true.
    ref = f"{registry}/{unique_repo}:v1"
    write_home_config(ocx, f'[managed]\nsource = "{ref}"\n')

    online = _run(ocx, "index", "catalog")
    assert online.returncode == 78, (
        f"required + no snapshot must exit 78 online, got {online.returncode}: {online.stderr}"
    )
    assert "ocx config update" in (online.stdout + online.stderr)

    offline = _run(ocx, "--offline", "index", "catalog")
    assert offline.returncode == 78, (
        f"required + no snapshot must exit 78 offline too (identical), got {offline.returncode}: {offline.stderr}"
    )


# ---------------------------------------------------------------------------
# Criterion 9 — env override is invocation-only: no file write; reverts
# ---------------------------------------------------------------------------


def test_env_only_override_never_writes_seed_and_reverts_without_env(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    _run(ocx, "index", "catalog", env_overrides={"OCX_MANAGED_CONFIG": ref})

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    assert not config_path.exists() or "[managed]" not in config_path.read_text(), (
        "OCX_MANAGED_CONFIG must never write the seed to config.toml"
    )

    reverted = _run(ocx, "index", "catalog")
    assert reverted.returncode == 0, "without the env override, no managed tier is active at all"


# ---------------------------------------------------------------------------
# Criterion 14 — `config update` bypasses the refresh throttle
# ---------------------------------------------------------------------------


def test_config_update_bypasses_throttle(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "throttle-before.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    assert _self_setup(ocx, ref, self_image).returncode == 0

    # Re-publish immediately -- well within the default 1-day refresh interval.
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "throttle-after.example"\n')

    update = _run(ocx, "config", "update")
    assert update.returncode == 0, f"config update must succeed: {update.stderr}"

    combined = _registry_probe(ocx)
    assert "throttle-after.example" in combined, (
        "`ocx config update` must bypass the refresh throttle and fetch immediately"
    )


# ---------------------------------------------------------------------------
# Criterion 17 — registry down during `config update`: error surfaces,
# existing snapshot untouched
# ---------------------------------------------------------------------------


def test_config_update_registry_down_leaves_snapshot_untouched(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "before-outage.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    assert _self_setup(ocx, ref, self_image).returncode == 0

    snapshot_path = Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "snapshot.json"
    before = snapshot_path.read_text()

    # Point the seed's source at an address nothing listens on (deterministic
    # connection-refused, no DNS dependency) without ever re-adopting via the
    # CLI -- direct file edit only mutates the plain TOML value the loader
    # reads; it does not need the fence's own dirty-detection to agree.
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    unreachable_ref = f"127.0.0.1:1/{unique_repo}:v1"
    config_path.write_text(config_path.read_text().replace(ref, unreachable_ref))

    result = _run(ocx, "config", "update")
    # A hard connection-refused against `127.0.0.1:1` fails inside the OCI
    # client's `GET /v2/` auth ping at the transport (connect) layer -- the
    # registry never answered, so it classifies as Unavailable (69), not a
    # credentials failure (80).
    assert result.returncode == 69, (
        f"a connection-refused update classifies as Unavailable (69), got {result.returncode}: {result.stderr}"
    )
    assert snapshot_path.read_text() == before, (
        "a failed update must never partially overwrite the existing snapshot"
    )


# ---------------------------------------------------------------------------
# Criterion 19 — CI ephemeral positive path: env-only + explicit update
# ---------------------------------------------------------------------------


def test_ci_env_only_recipe_no_fence_required_satisfied(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "ci-recipe.example"\n')

    update = _run(ocx, "config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref})
    assert update.returncode == 0, f"env-only `ocx config update` must succeed: {update.stderr}"

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    assert not config_path.exists() or "[managed]" not in config_path.read_text(), (
        "the CI recipe must never write a fence anywhere"
    )

    combined = _registry_probe(ocx, env_overrides={"OCX_MANAGED_CONFIG": ref})
    assert "ci-recipe.example" in combined, "the tier must merge for a later command carrying the same env var"


# ---------------------------------------------------------------------------
# Criterion 20 — setup fetch failure → no fence, no partial state
# ---------------------------------------------------------------------------


def test_setup_fetch_failure_no_fence_no_partial_state(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    bogus_ref = f"127.0.0.1:1/{unique_repo}:v1"
    result = _self_setup(ocx, bogus_ref, self_image)
    assert result.returncode != 0, "a fetch failure during onboard must not report success"

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    assert not config_path.exists() or "[managed]" not in config_path.read_text(), (
        "a failed onboard must never write the fence"
    )
    assert not (Path(ocx.env["OCX_HOME"]) / "state" / "managed-config").exists(), (
        "a failed onboard must never leave a partial snapshot directory"
    )

    follow_up = _run(ocx, "index", "catalog")
    assert follow_up.returncode == 0, "subsequent ordinary commands must be unaffected"


# ---------------------------------------------------------------------------
# Amended post-Codex-gate 2026-07-05 — a resolved source with no manifest in
# the registry (genuinely absent, not a network/auth fault) must exit 79 for
# both `ocx config update` and `ocx self setup --managed-config`, never a
# silent success or a panic.
# ---------------------------------------------------------------------------


def test_config_update_absent_ref_exits_79(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    write_home_config(ocx, f'[managed]\nsource = "{ref}"\n')

    result = _run(ocx, "config", "update")
    assert result.returncode == 79, (
        f"an absent-in-registry managed-config source must exit 79, got {result.returncode}: {result.stderr}"
    )


def test_setup_absent_ref_exits_79_no_fence_no_partial_state(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")

    result = _self_setup(ocx, ref, self_image)
    assert result.returncode == 79, (
        f"an absent-in-registry managed-config source must exit 79, got {result.returncode}: {result.stderr}"
    )

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    assert not config_path.exists() or "[managed]" not in config_path.read_text(), (
        "an absent-ref onboard must never write the fence"
    )
    assert not (Path(ocx.env["OCX_HOME"]) / "state" / "managed-config").exists(), (
        "an absent-ref onboard must never leave a partial snapshot directory"
    )

    follow_up = _run(ocx, "index", "catalog")
    assert follow_up.returncode == 0, "subsequent ordinary commands must be unaffected"


# ---------------------------------------------------------------------------
# Criterion 23 — managed fetch honors local [mirrors]; payload's own mirror
# entry for its own host is ignored for its own refresh
# ---------------------------------------------------------------------------


def test_managed_fetch_honors_local_mirror_ignores_payload_mirror(
    ocx: OcxRunner, registry: str, mirror_registry: str, unique_repo: str, tmp_path: Path
) -> None:
    # The managed ref lives on a FAKE canonical host so the local [mirrors]
    # entry that proves routing never shadows `{registry}` — the host the
    # bootstrap self-image is published to (setup phase 1 installs it via a
    # plain mirror-rewritten package install; shadowing that host with a
    # mirror that lacks the image would 404 the bootstrap before managed
    # config ever runs).
    managed_host = "managed-config.internal.test"
    ref = f"{managed_host}/{unique_repo}:v1"
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")

    # The artifact exists ONLY on the mirror -- a direct fetch of the fake
    # canonical host cannot even resolve. The payload itself embeds a hostile
    # [mirrors] entry redirecting its OWN host to the content-less primary
    # registry: if that entry were ever honored for the managed tier's OWN
    # refresh, a second update would 404 there.
    payload = (
        '[registry]\ndefault = "via-local-mirror.example"\n'
        f'[mirrors."{managed_host}"]\nurl = "http://{registry}"\n'
    ).encode()
    push_raw_config_package(mirror_registry, unique_repo, "v1", payload)

    write_home_config(ocx, f'[mirrors."{managed_host}"]\nurl = "http://{mirror_registry}"\n')
    env_overrides = {
        "OCX_INSECURE_REGISTRIES": f"{registry},{mirror_registry},{managed_host}"
    }

    first = _self_setup(ocx, ref, self_image, env_overrides=env_overrides)
    assert first.returncode == 0, (
        f"setup must succeed via the locally-configured mirror (artifact absent upstream): {first.stderr}"
    )

    second = _run(ocx, "config", "update", env_overrides=env_overrides)
    assert second.returncode == 0, (
        f"a second update must still route through the LOCAL mirror map, never the "
        f"payload's own poisoned [mirrors] entry: {second.stderr}"
    )


# ---------------------------------------------------------------------------
# Criterion 25 — OCX_NO_CONFIG_REFRESH kills the tick, not explicit update
# ---------------------------------------------------------------------------


def test_no_config_refresh_kill_switch_silences_debug_hook_but_not_explicit_update(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    # Half A: the kill-switch check in the background-tick hook runs BEFORE
    # the TTY gate, so its debug log fires even in a non-interactive
    # subprocess (app/managed_config_check.rs::check_for_managed_config_refresh).
    silenced = _run(ocx, "index", "catalog", env_overrides={"OCX_NO_CONFIG_REFRESH": "1"}, log_level="debug")
    assert "OCX_NO_CONFIG_REFRESH" in silenced.stderr, (
        f"the kill switch must be observable in the debug log, got stderr: {silenced.stderr!r}"
    )

    # Half B: the kill switch must NOT block the EXPLICIT `ocx config update`
    # verb -- only the automatic background tick.
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "kill-switch-a.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    setup = _self_setup(ocx, ref, self_image, env_overrides={"OCX_NO_CONFIG_REFRESH": "1"})
    assert setup.returncode == 0, f"onboard under the kill switch must still succeed: {setup.stderr}"

    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "kill-switch-b.example"\n')
    update = _run(ocx, "config", "update", env_overrides={"OCX_NO_CONFIG_REFRESH": "1"})
    assert update.returncode == 0, f"explicit `ocx config update` must still work under the kill switch: {update.stderr}"


# ---------------------------------------------------------------------------
# Criterion 26 — OCX_NO_CONFIG is fully hermetic
# ---------------------------------------------------------------------------


def test_no_config_hermetic_suppresses_candidate_and_env_override(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(
        registry, unique_repo, "v1", b'[registry]\ndefault = "should-be-ignored-managed.example"\n'
    )
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    assert _self_setup(ocx, ref, self_image).returncode == 0

    combined = _registry_probe(ocx, env_overrides={"OCX_NO_CONFIG": "1", "OCX_MANAGED_CONFIG": ref})
    assert "should-be-ignored-managed.example" not in combined, (
        "OCX_NO_CONFIG=1 must suppress the managed-config tier (candidate AND env override)"
    )


# ---------------------------------------------------------------------------
# Criterion 27 — zip-and-move warm HOME: identical resolved config offline;
# digest-pin ref round-trips
# ---------------------------------------------------------------------------


def test_zip_and_move_warm_home_offline_identical(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    digest = push_raw_config_package(
        registry, unique_repo, "v1", b'[registry]\ndefault = "warm-home.example"\n'
    )
    ref = f"{registry}/{unique_repo}@{digest}"
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    setup = _self_setup(ocx, ref, self_image)
    assert setup.returncode == 0, f"digest-pinned onboard must succeed: {setup.stderr}"

    moved_home = tmp_path / "moved-ocx-home"
    shutil.copytree(ocx.env["OCX_HOME"], moved_home, symlinks=True)

    moved_env = {**ocx.env, "OCX_HOME": str(moved_home)}
    moved_env.pop("OCX_DEFAULT_REGISTRY", None)
    result = subprocess.run(
        [str(ocx.binary), "--format", "json", "--offline", "package", "install", "nonexistent_pkg_ocx_test:0"],
        capture_output=True,
        text=True,
        env=moved_env,
    )
    combined = result.stdout + result.stderr
    assert "warm-home.example" in combined, (
        f"a moved, warm HOME must resolve the managed tier fully offline: {combined!r}"
    )


# ---------------------------------------------------------------------------
# Criterion 29 — `config update --check` probe
# ---------------------------------------------------------------------------


def test_config_update_check_probe_reports_status_and_json_via_root_format(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "probe.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    assert _self_setup(ocx, ref, self_image).returncode == 0

    check = _run(ocx, "config", "update", "--check")
    assert check.returncode == 0, f"--check must never swap or fail on a healthy tier: {check.stderr}"
    payload = json.loads(check.stdout)
    assert payload.get("status") in ("already_current", "checked"), payload
    assert payload.get("source") == ref, payload
    assert "digest" in payload, payload

    # Plain (no --format json) must also succeed -- JSON is a root --format
    # concern, not a subcommand-level flag (criterion 29).
    plain = _run(ocx, "config", "update", "--check", format=None)
    assert plain.returncode == 0, f"plain-format --check must also succeed: {plain.stderr}"


# ---------------------------------------------------------------------------
# Gate v2 (managed-config v2) — tag float, digest pin, self-heal, clear
# ---------------------------------------------------------------------------


def test_snapshot_survives_tag_float(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    """Gate v2: the snapshot identity is registry/repository — a snapshot
    synced under one tag still satisfies the required gate when the effective
    source tracks a different tag on the same repository."""
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "float-a.example"\n')
    push_raw_config_package(registry, unique_repo, "v2", b'[registry]\ndefault = "float-b.example"\n')

    ref_v1 = f"{registry}/{unique_repo}:v1"
    ref_v2 = f"{registry}/{unique_repo}:v2"
    assert _run(ocx, "config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref_v1}).returncode == 0

    # An ordinary command under the v2 tag must NOT fail the required gate --
    # the v1-tagged snapshot satisfies it (tags float within one repository).
    probe = _run(
        ocx, "package", "install", "nonexistent_pkg_ocx_test:0",
        env_overrides={"OCX_MANAGED_CONFIG": ref_v2},
    )
    assert probe.returncode != 78, (
        f"a same-repo snapshot under another tag must satisfy the required gate: {probe.stderr}"
    )
    # The stale-but-matching snapshot's content still merges until an update.
    combined = _registry_probe(ocx, env_overrides={"OCX_MANAGED_CONFIG": ref_v2})
    assert "float-a.example" in combined, (
        f"the tag-floated snapshot content must keep merging until the next update: {combined!r}"
    )


def test_digest_pinned_seed_fails_closed_on_other_digest(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    """Gate v2 clause 2: a digest-pinned source binds — a snapshot with any
    other digest is treated as absent (required gate exits 78)."""
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "pin.example"\n')
    ref = f"{registry}/{unique_repo}:v1"
    assert _run(ocx, "config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref}).returncode == 0

    snapshot_path = Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "snapshot.json"
    snapshot_digest = json.loads(snapshot_path.read_text())["digest"]

    # Matching pin: the required gate is satisfied.
    pinned_ok = f"{registry}/{unique_repo}@{snapshot_digest}"
    ok = _run(
        ocx, "package", "install", "nonexistent_pkg_ocx_test:0",
        env_overrides={"OCX_MANAGED_CONFIG": pinned_ok},
    )
    assert ok.returncode != 78, f"a matching digest pin must satisfy the gate: {ok.stderr}"

    # Any other digest: fail closed.
    other = f"{registry}/{unique_repo}@sha256:{'b' * 64}"
    closed = _run(
        ocx, "package", "install", "nonexistent_pkg_ocx_test:0",
        env_overrides={"OCX_MANAGED_CONFIG": other},
    )
    assert closed.returncode == 78, (
        f"a digest-pinned seed must fail closed on a different snapshot digest, got {closed.returncode}: "
        f"{closed.stderr}"
    )


# ---------------------------------------------------------------------------
# S6 — a digest-pinned [managed] seed + `config update <other-version>` warns
# loudly that the sync will brick the required gate until the seed pin moves.
# ---------------------------------------------------------------------------


def test_config_update_version_against_digest_pinned_seed_warns_bricked_gate(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    digest_v1 = push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "pinned-v1.example"\n')
    digest_v2 = push_raw_config_package(registry, unique_repo, "v2", b'[registry]\ndefault = "other-v2.example"\n')
    assert digest_v1 != digest_v2, "the two versions must have distinct index digests for the warning to fire"

    # The seed pins the tier to v1's exact digest; the required gate binds to it.
    write_home_config(ocx, f'[managed]\nsource = "{registry}/{unique_repo}@{digest_v1}"\n')

    # Explicitly sync a different version — the update succeeds but the synced
    # digest no longer matches the seed pin, so ordinary commands would fail the
    # required gate until the seed pin is updated. `config update` warns loudly.
    result = _run(ocx, "config", "update", "v2")
    assert result.returncode == 0, f"the update itself must succeed: {result.stderr}"
    assert "fail the required gate" in result.stderr, (
        f"a digest-pinned seed synced to a different version must warn about the bricked gate; "
        f"got stderr: {result.stderr!r}"
    )


def test_setup_self_heals_wiped_snapshot(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """W3: a re-run of `self setup --managed-config <same-ref>` with a Current
    fence but a wiped snapshot re-fetches instead of reporting a false
    already-adopted."""
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "heal.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    assert _self_setup(ocx, ref, self_image).returncode == 0

    state_dir = Path(ocx.env["OCX_HOME"]) / "state" / "managed-config"
    shutil.rmtree(state_dir)

    heal = _self_setup(ocx, ref, self_image)
    assert heal.returncode == 0, f"self-heal re-run must succeed: {heal.stderr}"
    assert (state_dir / "snapshot.json").exists(), (
        "a Current fence with a wiped snapshot must re-fetch and re-persist"
    )


# ---------------------------------------------------------------------------
# S5 — `self setup --managed-config <ref> --dry-run` reports would-adopt and
# writes nothing (no snapshot, no fence).
# ---------------------------------------------------------------------------


def test_setup_managed_config_dry_run_reports_would_adopt_writes_nothing(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "dry-run.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")

    result = _self_setup(ocx, ref, self_image, "--dry-run")
    assert result.returncode == 0, f"a dry-run onboard must succeed: {result.stderr}"

    payload = json.loads(result.stdout)
    assert payload["managed_config"]["status"] == "would_adopt", payload

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    assert not config_path.exists() or "[managed]" not in config_path.read_text(), (
        "a dry-run must never write the [managed] fence"
    )
    assert not (Path(ocx.env["OCX_HOME"]) / "state" / "managed-config").exists(), (
        "a dry-run must never fetch or persist a snapshot"
    )


# ---------------------------------------------------------------------------
# F2 — bare `self setup` (no --managed-config flag) resolves the tier from the
# OCX_MANAGED_CONFIG env var (a) or the existing [managed] seed (b).
#
# These pin the env/seed fallback resolution being wired in parallel
# (setup::apply_managed_config currently short-circuits to `not_configured`
# when the flag is absent); expect them to FAIL until that lands.
# ---------------------------------------------------------------------------


def _bare_self_setup(
    ocx: OcxRunner, self_image: str, *, env_overrides: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    """Runs `ocx self setup --no-modify-path` WITHOUT `--managed-config`, so the
    managed tier can only come from OCX_MANAGED_CONFIG or the existing seed."""
    overrides = {"__OCX_SELF_IMAGE": self_image}
    if env_overrides:
        overrides.update(env_overrides)
    return _run(ocx, "self", "setup", "--no-modify-path", env_overrides=overrides)


def test_env_only_bare_self_setup_adopts_tier(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "env-bare-adopt.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")

    result = _bare_self_setup(ocx, self_image, env_overrides={"OCX_MANAGED_CONFIG": ref})
    assert result.returncode == 0, f"bare setup under OCX_MANAGED_CONFIG must succeed: {result.stderr}"

    payload = json.loads(result.stdout)
    assert payload["managed_config"]["status"] in ("adopted", "already_adopted"), payload
    assert (Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "snapshot.json").exists(), (
        "OCX_MANAGED_CONFIG + bare `self setup` must fetch and persist a snapshot"
    )


def test_seeded_bare_self_setup_self_heals_deleted_snapshot(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "seed-bare-heal.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")

    # First adopt via the explicit flag to write the [managed] seed + snapshot.
    assert _self_setup(ocx, ref, self_image).returncode == 0
    state_dir = Path(ocx.env["OCX_HOME"]) / "state" / "managed-config"
    shutil.rmtree(state_dir)

    # Bare re-run: no flag, no env — the [managed] seed alone must drive a
    # self-heal that re-fetches and re-persists the snapshot.
    result = _bare_self_setup(ocx, self_image)
    assert result.returncode == 0, f"bare re-run must self-heal from the seed: {result.stderr}"

    payload = json.loads(result.stdout)
    assert payload["managed_config"]["status"] in ("adopted", "already_adopted"), payload
    assert (state_dir / "snapshot.json").exists(), (
        "the seed-driven bare re-run must re-persist the wiped snapshot"
    )


def test_clear_removes_tier(ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path) -> None:
    """W5: `self setup --managed-config ""` removes the fence AND the snapshot
    directory; the tier no longer merges afterwards."""
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "cleared.example"\n')
    self_image = _publish_self_image(ocx, tmp_path, f"{unique_repo}_self")
    assert _self_setup(ocx, ref, self_image).returncode == 0

    clear = _self_setup(ocx, "", self_image)
    assert clear.returncode == 0, f"clear must succeed: {clear.stderr}"

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    assert not config_path.exists() or "[managed]" not in config_path.read_text(), (
        "the [managed] fence must be removed by a clear"
    )
    assert not (Path(ocx.env["OCX_HOME"]) / "state" / "managed-config").exists(), (
        "the snapshot directory must be deleted by a clear"
    )
    combined = _registry_probe(ocx)
    assert "cleared.example" not in combined, (
        f"the cleared tier must no longer merge into any command: {combined!r}"
    )


# ---------------------------------------------------------------------------
# `config update [VERSION]` + `--pause` / `--resume` (managed-config v2 phase 4)
# ---------------------------------------------------------------------------


def _pause_file(ocx: OcxRunner) -> Path:
    return Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "pause.json"


def _snapshot_digest(ocx: OcxRunner) -> str:
    snapshot_path = Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "snapshot.json"
    return json.loads(snapshot_path.read_text())["digest"]


def test_config_update_version_pin_rolls_back_and_survives_required_gate(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    digest_old = push_managed_config(
        ocx, unique_repo, "user-1.0.0", '[registry]\ndefault = "pin-old.example"\n', tmp_path, cascade=True
    )
    digest_new = push_managed_config(
        ocx, unique_repo, "user-1.0.1", '[registry]\ndefault = "pin-new.example"\n', tmp_path,
        cascade=True, new=False,
    )
    ref = f"{registry}/{unique_repo}:user"
    env = {"OCX_MANAGED_CONFIG": ref}

    # Bare update follows the floating tag to the newest version.
    update = _run(ocx, "config", "update", env_overrides=env)
    assert update.returncode == 0, update.stderr
    assert json.loads(update.stdout)["digest"] == digest_new

    # Explicit VERSION rolls back to the older version.
    rollback = _run(ocx, "config", "update", "user-1.0.0", env_overrides=env)
    assert rollback.returncode == 0, rollback.stderr
    payload = json.loads(rollback.stdout)
    assert payload["status"] == "updated"
    assert payload["digest"] == digest_old
    assert payload.get("tag") == "user-1.0.0"
    assert _snapshot_digest(ocx) == digest_old

    # The pinned (rolled-back) snapshot still satisfies the required gate for
    # the floating seed (gate v2: tags float within one repository).
    probe = _run(ocx, "package", "install", "nonexistent_pkg_ocx_test:0", env_overrides=env)
    assert probe.returncode != 78, f"a version-pinned snapshot must survive the required gate: {probe.stderr}"


def test_config_update_tag_at_digest_mismatch_exits_65_snapshot_untouched(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "assert.example"\n')
    ref = f"{registry}/{unique_repo}:v1"
    env = {"OCX_MANAGED_CONFIG": ref}
    assert _run(ocx, "config", "update", env_overrides=env).returncode == 0
    before = _snapshot_digest(ocx)

    mismatch = _run(ocx, "config", "update", f"v1@sha256:{'b' * 64}", env_overrides=env)
    assert mismatch.returncode == 65, (
        f"a tag@digest mismatch must fail closed with 65, got {mismatch.returncode}: {mismatch.stderr}"
    )
    assert _snapshot_digest(ocx) == before, "a failed assertion must leave the snapshot untouched"

    # The matching pin succeeds.
    ok = _run(ocx, "config", "update", f"v1@{before}", env_overrides=env)
    assert ok.returncode == 0, f"a matching tag@digest pin must succeed: {ok.stderr}"


def test_config_update_pause_writes_and_bare_update_clears(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "pause.example"\n')
    ref = f"{registry}/{unique_repo}:v1"
    env = {"OCX_MANAGED_CONFIG": ref}
    assert _run(ocx, "config", "update", env_overrides=env).returncode == 0

    # `--pause` without VERSION: no fetch, pause recorded.
    paused = _run(ocx, "config", "update", "--pause", "4h", env_overrides=env)
    assert paused.returncode == 0, paused.stderr
    payload = json.loads(paused.stdout)
    assert payload.get("paused_until"), payload
    assert _pause_file(ocx).exists()

    # `--check` reports the pause without touching it.
    check = _run(ocx, "config", "update", "--check", env_overrides=env)
    assert check.returncode == 0
    check_payload = json.loads(check.stdout)
    assert check_payload.get("paused_until"), check_payload
    assert _pause_file(ocx).exists(), "--check must never modify the pause file"

    # A bare explicit update clears the pause.
    bare = _run(ocx, "config", "update", env_overrides=env)
    assert bare.returncode == 0
    assert not _pause_file(ocx).exists(), "an explicit update without --pause must clear the pause"


def test_config_update_pause_with_version_pins_then_pauses(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    digest_old = push_managed_config(
        ocx, unique_repo, "user-1.0.0", '[registry]\ndefault = "hold-old.example"\n', tmp_path, cascade=True
    )
    push_managed_config(
        ocx, unique_repo, "user-1.0.1", '[registry]\ndefault = "hold-new.example"\n', tmp_path,
        cascade=True, new=False,
    )
    ref = f"{registry}/{unique_repo}:user"
    env = {"OCX_MANAGED_CONFIG": ref}

    held = _run(ocx, "config", "update", "--pause", "3d", "user-1.0.0", env_overrides=env)
    assert held.returncode == 0, held.stderr
    payload = json.loads(held.stdout)
    assert payload["digest"] == digest_old
    assert payload.get("paused_until"), payload
    assert payload.get("pinned") == "user-1.0.0", payload
    pause = json.loads(_pause_file(ocx).read_text())
    assert pause["pinned_version"] == "user-1.0.0"

    check = _run(ocx, "config", "update", "--check", env_overrides=env)
    check_payload = json.loads(check.stdout)
    assert check_payload.get("pinned") == "user-1.0.0", check_payload
    assert check_payload.get("paused_until"), check_payload


def test_config_update_resume_clears_pause_and_syncs(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "resume-a.example"\n')
    ref = f"{registry}/{unique_repo}:v1"
    env = {"OCX_MANAGED_CONFIG": ref}
    assert _run(ocx, "config", "update", "--pause", "3d", env_overrides=env).returncode == 0
    assert _pause_file(ocx).exists()

    digest_new = push_raw_config_package(
        registry, unique_repo, "v1", b'[registry]\ndefault = "resume-b.example"\n'
    )
    resumed = _run(ocx, "config", "update", "--resume", env_overrides=env)
    assert resumed.returncode == 0, resumed.stderr
    assert not _pause_file(ocx).exists(), "--resume must clear the pause"
    assert json.loads(resumed.stdout)["digest"] == digest_new, "--resume must sync to the registry's current state"


def test_config_update_resume_registry_down_keeps_pause(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    """S8: `--resume` clears the pause only after a successful sync. When the
    registry is unreachable the command fails and the pause file survives, so a
    later `--resume` can still lift the hold once the registry recovers."""
    push_raw_config_package(registry, unique_repo, "v1", b'[registry]\ndefault = "resume-down.example"\n')
    ref = f"{registry}/{unique_repo}:v1"
    env = {"OCX_MANAGED_CONFIG": ref}

    # Adopt (snapshot) then record a pause against the reachable ref.
    assert _run(ocx, "config", "update", env_overrides=env).returncode == 0
    assert _run(ocx, "config", "update", "--pause", "3d", env_overrides=env).returncode == 0
    assert _pause_file(ocx).exists()

    # Resume while the tier's source points at an address nothing listens on.
    down_env = {"OCX_MANAGED_CONFIG": f"127.0.0.1:1/{unique_repo}:v1"}
    resumed = _run(ocx, "config", "update", "--resume", env_overrides=down_env)
    assert resumed.returncode != 0, "a registry-down resume must fail, not report success"
    assert _pause_file(ocx).exists(), (
        "the pause is cleared only after a successful update, so a failed resume must leave it in force"
    )


def test_config_update_flag_conflicts_exit_64(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    env = {"OCX_MANAGED_CONFIG": ref}
    for args in (
        ("--check", "--pause", "1h"),
        ("--check", "--resume"),
        ("--check", "v1"),
        ("--resume", "--pause", "1h"),
        ("--resume", "v1"),
    ):
        result = _run(ocx, "config", "update", *args, env_overrides=env)
        assert result.returncode == 64, (
            f"conflicting flags {args} must exit 64, got {result.returncode}: {result.stderr}"
        )


def test_config_update_pause_over_cap_exits_64(ocx: OcxRunner, unique_repo: str, registry: str) -> None:
    ref = f"{registry}/{unique_repo}:v1"
    env = {"OCX_MANAGED_CONFIG": ref}
    over_cap = _run(ocx, "config", "update", "--pause", "8d", env_overrides=env)
    assert over_cap.returncode == 64, f"an over-cap pause must exit 64, got {over_cap.returncode}"
    malformed = _run(ocx, "config", "update", "--pause", "soon", env_overrides=env)
    assert malformed.returncode == 64, f"a malformed pause must exit 64, got {malformed.returncode}"
