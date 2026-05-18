"""Specification tests for the StateProvider registry (SP0–SP6) and
display-env seam accessor (DE0–DE4, DE6).

Tests written from design_spec_doc_command_scripts.md §3 and §6f — NOT from
the stub implementation.  They MUST fail against the current stub (raise
NotImplementedError / empty DECLARED_PACKAGES table) and pass only once
Phase-1 implementation lands.

Contract reference:
  SP0 — import triggers zero registry I/O
  SP1 — explicit-family resolution; separate namespaces, no collision
  SP2 — provider.packages is dict[str, PackageInfo] keyed by display name
  SP3 — provider.script_env() returns Scenario env projection
  SP4 — provider.display_map() returns (sanitize_map, repo_map) inverse pair
  SP5 — SetupAdapter over each SETUPS name is behaviour-equivalent to legacy fn
  SP6 — ScenarioAdapter over each SCENARIOS key exposes same $PKG_* projection

  DE0 — Hat-1 oracle: pins post-provision provider.packages short refs for
        every SETUPS name and Scenario subclass; baseline for DE3/DE6.
        Committed first. Expected to PASS today (provision works; only
        declared_display_env() is the stub — but DE0 does not call it).
  DE2 — declared_display_env() is static zero-I/O: returns PKG_<KEY>→short_ref
        from module-level DECLARED_PACKAGES; MUST raise NotImplementedError
        until the Implement phase fills the table.
  DE3 — declared_display_env() values == DE0 oracle PKG_<KEY> projection;
        FAILS until DE2 is implemented.
  DE4 — SP0 extended: declared_display_env() + doc_scripts_export perform
        zero registry/network I/O; FAILS until DE2 is implemented.
  DE6 — Provisioned cross-check: after provision(), declared_display_env()
        keys+values == PKG_<KEY>→pkg.short projection of provider.packages;
        no skip/opt-in; FAILS until DE2 is implemented.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from uuid import uuid4

import re

import pytest

from recordings.setups import SETUPS
from src.runner import OcxRunner, PackageInfo
from src.scenarios import SCENARIOS, Scenario

# SP7 parallel-isolation prefix: ``t_<8hex>_`` (SetupAdapter) /
# ``s_<8hex>_`` (ScenarioAdapter) is injected into the actual repo name and
# therefore appears in ``PackageInfo.short``.  ``declared_display_env()``
# returns the **canonical** (prefix-free) short a reader types — so DE3/DE6
# compare ``declared`` against the prefix-stripped provisioned short, NOT the
# raw ``pkg.short`` (Living Design Record 2026-05-17; design spec §6f DE3/DE6).
_SP7_PREFIX = re.compile(r"^[ts]_[0-9a-f]{8}_")


def _canonical_proj(packages: dict[str, PackageInfo]) -> dict[str, str]:
    """Renderable-var matrix — provisioned truth with the SP7 isolation
    prefix stripped (the values a reader should see / type):

    - ``PKG_<KEY>``  → canonical short  (``p.short`` minus ``[ts]_<8hex>_``)
    - ``REPO_<KEY>`` → canonical repo   (``p.repo``  minus ``[ts]_<8hex>_``)

    Mirrors ``state_providers._project_declared_display_env`` (LDR
    2026-05-17: PKG_ + REPO_ are the two renderable forms; DE3/DE6 verify
    declared == this)."""
    out: dict[str, str] = {}
    for k, p in packages.items():
        key = k.upper().replace("-", "_")
        out[f"PKG_{key}"] = _SP7_PREFIX.sub("", p.short)
        out[f"REPO_{key}"] = _SP7_PREFIX.sub("", p.repo)
    return out

# Shell scenarios + registry I/O are Linux/macOS only; parity with
# test_scenarios_smoke.py (see .claude/rules/subsystem-tests.md "Platform Split").
pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="Shell scenarios target Linux/macOS; Windows coverage in the pytest suite.",
)

# ---------------------------------------------------------------------------
# SP0 — importing state_providers performs zero registry I/O
# ---------------------------------------------------------------------------


def test_sp0_import_performs_zero_registry_io() -> None:
    """SP0: importing src.state_providers + accessing STATE_PROVIDERS must not
    trigger any network / OCI push.

    Executed in a subprocess that has no registry fixture — any attempted
    network call would either raise a ConnectionRefusedError or timeout before
    the assertion.  The test only asserts that the import + attribute access
    completes successfully (exit 0) and that STATE_PROVIDERS is a dict whose
    keys are all fully-qualified ('setup:…' / 'scenario:…').
    """
    code = """
import sys
from src import state_providers

# Accessing STATE_PROVIDERS must not call provision()
registry = state_providers.STATE_PROVIDERS

# Must be a non-empty dict (all 7 + 6 families registered at import time)
assert isinstance(registry, dict), f"STATE_PROVIDERS is {type(registry)}, expected dict"
assert len(registry) >= 13, (
    f"STATE_PROVIDERS has {len(registry)} entries; expected at least 13 "
    f"(7 setup + 6 scenario families)"
)

# All keys must be family-qualified
for key in registry:
    assert key.startswith("setup:") or key.startswith("scenario:"), (
        f"Unqualified key in STATE_PROVIDERS: {key!r}"
    )

print("OK")
"""
    result = subprocess.run(
        [sys.executable, "-c", code],
        capture_output=True,
        text=True,
        cwd=str(Path(__file__).parent.parent),  # test/ root on PYTHONPATH
    )
    assert result.returncode == 0, (
        f"import test failed (rc={result.returncode}):\n"
        f"stdout: {result.stdout}\n"
        f"stderr: {result.stderr}"
    )
    assert "OK" in result.stdout


def test_sp0_state_providers_populated_for_all_families() -> None:
    """SP0: STATE_PROVIDERS must contain entries for all 7 SETUPS + 6 SCENARIOS
    families WITHOUT a registry fixture — construction is pure, no I/O.
    """
    from src.state_providers import STATE_PROVIDERS

    setup_keys = {k for k in STATE_PROVIDERS if k.startswith("setup:")}
    scenario_keys = {k for k in STATE_PROVIDERS if k.startswith("scenario:")}

    expected_setup_keys = {f"setup:{name}" for name in SETUPS}
    expected_scenario_keys = {f"scenario:{name}" for name in SCENARIOS}

    assert setup_keys == expected_setup_keys, (
        f"Setup family keys mismatch.\n"
        f"  expected: {sorted(expected_setup_keys)}\n"
        f"  actual:   {sorted(setup_keys)}"
    )
    assert scenario_keys == expected_scenario_keys, (
        f"Scenario family keys mismatch.\n"
        f"  expected: {sorted(expected_scenario_keys)}\n"
        f"  actual:   {sorted(scenario_keys)}"
    )


# ---------------------------------------------------------------------------
# SP1 — explicit-family resolution; no collision; EX4 message form
# ---------------------------------------------------------------------------


def test_sp1_setup_basic_resolves_to_setup_adapter() -> None:
    """SP1: resolve_state('setup:basic') returns a SetupAdapter instance."""
    from src.state_providers import SetupAdapter, resolve_state

    provider = resolve_state("setup:basic")
    assert isinstance(provider, SetupAdapter), (
        f"resolve_state('setup:basic') returned {type(provider).__name__}, "
        f"expected SetupAdapter"
    )


def test_sp1_scenario_basic_raises_value_error() -> None:
    """SP1: resolve_state('scenario:basic') raises ValueError — no 'basic' key
    in the scenario family (scenario keys are PascalCase class names).
    """
    from src.state_providers import resolve_state

    with pytest.raises(ValueError, match="invalid state"):
        resolve_state("scenario:basic")


def test_sp1_unqualified_name_raises_value_error_with_ex4_message() -> None:
    """SP1 / EX4: an unqualified state string (no setup:/scenario: prefix) must
    raise ValueError with the message form:
    'invalid state '<v>'; expected setup:<name> or scenario:<Name>; available: …'
    """
    from src.state_providers import resolve_state

    with pytest.raises(ValueError) as exc_info:
        resolve_state("basic")

    message = str(exc_info.value)
    assert "invalid state 'basic'" in message, (
        f"ValueError message missing EX4 form: {message!r}"
    )
    assert "setup:<name>" in message or "setup:" in message, (
        f"ValueError message missing 'setup:<name>': {message!r}"
    )
    assert "scenario:" in message, (
        f"ValueError message missing 'scenario:': {message!r}"
    )
    assert "available:" in message, (
        f"ValueError message missing 'available:': {message!r}"
    )


def test_sp1_scenario_basic_package_resolves_to_scenario_adapter() -> None:
    """SP1: resolve_state('scenario:BasicPackage') returns a ScenarioAdapter."""
    from src.state_providers import ScenarioAdapter, resolve_state

    provider = resolve_state("scenario:BasicPackage")
    assert isinstance(provider, ScenarioAdapter), (
        f"resolve_state('scenario:BasicPackage') returned {type(provider).__name__}, "
        f"expected ScenarioAdapter"
    )


def test_sp1_setup_and_scenario_adapters_are_distinct_objects() -> None:
    """SP1: setup:basic and any scenario adapter are DISTINCT objects — no union."""
    from src.state_providers import ScenarioAdapter, SetupAdapter, resolve_state

    setup_provider = resolve_state("setup:basic")
    scenario_provider = resolve_state("scenario:BasicPackage")

    assert setup_provider is not scenario_provider, (
        "setup:basic and scenario:BasicPackage must be distinct objects"
    )
    assert isinstance(setup_provider, SetupAdapter)
    assert isinstance(scenario_provider, ScenarioAdapter)


# ---------------------------------------------------------------------------
# SP2 — provider.packages is dict[str, PackageInfo] keyed by display name
# (post-provision)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp2_dependencies_packages_keys_post_provision(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP2: setup:dependencies.packages after provision() is keyed by
    display names {nodejs, bun, webapp}.
    """
    from src.state_providers import resolve_state

    provider = resolve_state("setup:dependencies")
    provider.provision(ocx, tmp_path)

    assert isinstance(provider.packages, dict), (
        f"provider.packages is {type(provider.packages)}, expected dict"
    )
    assert set(provider.packages.keys()) == {"nodejs", "bun", "webapp"}, (
        f"setup:dependencies packages keys: {sorted(provider.packages.keys())}"
    )
    for display_name, pkg in provider.packages.items():
        assert isinstance(pkg, PackageInfo), (
            f"packages[{display_name!r}] is {type(pkg)}, expected PackageInfo"
        )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp2_two_level_deps_packages_keys_post_provision(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP2: scenario:TwoLevelDeps.packages after provision() is keyed by
    display names {leaf, app}.
    """
    from src.state_providers import resolve_state

    provider = resolve_state("scenario:TwoLevelDeps")
    provider.provision(ocx, tmp_path)

    assert isinstance(provider.packages, dict)
    assert set(provider.packages.keys()) == {"leaf", "app"}, (
        f"scenario:TwoLevelDeps packages keys: {sorted(provider.packages.keys())}"
    )
    for display_name, pkg in provider.packages.items():
        assert isinstance(pkg, PackageInfo), (
            f"packages[{display_name!r}] is {type(pkg)}, expected PackageInfo"
        )


# ---------------------------------------------------------------------------
# SP3 — provider.script_env() returns Scenario env projection (post-provision)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp3_scenario_basic_package_script_env_contains_expected_vars(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP3: scenario:BasicPackage.script_env() post-provision contains
    PKG_HELLO, FQ_HELLO, REPO_HELLO, TAG_HELLO, MARKER_HELLO, OCX, OCX_HOME,
    REGISTRY.
    """
    from src.state_providers import resolve_state

    provider = resolve_state("scenario:BasicPackage")
    provider.provision(ocx, tmp_path)

    env = provider.script_env()
    assert isinstance(env, dict), f"script_env() returned {type(env)}, expected dict"

    per_pkg_vars = ["PKG_HELLO", "FQ_HELLO", "REPO_HELLO", "TAG_HELLO", "MARKER_HELLO"]
    runner_vars = ["OCX", "OCX_HOME", "REGISTRY"]

    for var in per_pkg_vars + runner_vars:
        assert var in env, (
            f"scenario:BasicPackage script_env() missing {var!r}"
        )
        assert env[var], (
            f"scenario:BasicPackage script_env()[{var!r}] is empty"
        )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp3_setup_dependencies_script_env_contains_per_package_vars(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP3: setup:dependencies.script_env() (SetupAdapter MUST synthesize the
    Scenario projection) contains PKG_NODEJS, PKG_BUN, PKG_WEBAPP and the
    runner-level vars OCX, OCX_HOME, REGISTRY.

    KEY derivation rule: display name uppercased, hyphens → underscores.
    """
    from src.state_providers import resolve_state

    provider = resolve_state("setup:dependencies")
    provider.provision(ocx, tmp_path)

    env = provider.script_env()
    assert isinstance(env, dict)

    per_pkg_vars = ["PKG_NODEJS", "PKG_BUN", "PKG_WEBAPP"]
    runner_vars = ["OCX", "OCX_HOME", "REGISTRY"]

    for var in per_pkg_vars + runner_vars:
        assert var in env, (
            f"setup:dependencies script_env() missing {var!r}"
        )
        assert env[var], (
            f"setup:dependencies script_env()[{var!r}] is empty"
        )


# ---------------------------------------------------------------------------
# SP4 — provider.display_map() returns (sanitize_map, repo_map) inverse pair
# (post-provision)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp4_display_map_structure_and_inverse(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP4: display_map() returns a 2-tuple (sanitize_map, repo_map).

    sanitize_map = {actual_repo: display_name}
    repo_map     = {display_name: actual_repo}

    They must be inverses of each other for every entry.
    Uses scenario:BasicPackage for a minimal single-package assertion.
    """
    from src.state_providers import resolve_state

    provider = resolve_state("scenario:BasicPackage")
    provider.provision(ocx, tmp_path)

    result = provider.display_map()
    assert isinstance(result, tuple) and len(result) == 2, (
        f"display_map() returned {type(result)} of length "
        f"{len(result) if isinstance(result, (tuple, list)) else '?'}, "
        f"expected 2-tuple"
    )

    sanitize_map, repo_map = result

    assert isinstance(sanitize_map, dict), (
        f"sanitize_map is {type(sanitize_map)}, expected dict"
    )
    assert isinstance(repo_map, dict), (
        f"repo_map is {type(repo_map)}, expected dict"
    )

    # Both non-empty after provision
    assert sanitize_map, "sanitize_map is empty after provision()"
    assert repo_map, "repo_map is empty after provision()"

    # They must be exact inverses: {A→B} inverse is {B→A}
    for actual_repo, display_name in sanitize_map.items():
        assert display_name in repo_map, (
            f"display_name {display_name!r} from sanitize_map not found as key in repo_map"
        )
        assert repo_map[display_name] == actual_repo, (
            f"repo_map[{display_name!r}]={repo_map[display_name]!r} != "
            f"sanitize_map inverse {actual_repo!r}"
        )

    for display_name, actual_repo in repo_map.items():
        assert actual_repo in sanitize_map, (
            f"actual_repo {actual_repo!r} from repo_map not found as key in sanitize_map"
        )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp4_display_map_key_value_semantics(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP4: sanitize_map keys are actual_repo names; values are display names.
    repo_map keys are display names; values are actual_repo names.

    BasicPackage publishes 'hello' under a UUID-prefixed repo — the display
    name is 'hello' and the actual repo has the UUID prefix.
    """
    from src.state_providers import resolve_state

    provider = resolve_state("scenario:BasicPackage")
    provider.provision(ocx, tmp_path)

    sanitize_map, repo_map = provider.display_map()
    pkg = provider.packages["hello"]

    # sanitize_map: actual_repo → display_name
    assert pkg.repo in sanitize_map, (
        f"actual_repo {pkg.repo!r} not in sanitize_map keys: {sorted(sanitize_map)}"
    )
    assert sanitize_map[pkg.repo] == "hello", (
        f"sanitize_map[{pkg.repo!r}] = {sanitize_map[pkg.repo]!r}, expected 'hello'"
    )

    # repo_map: display_name → actual_repo
    assert "hello" in repo_map, (
        f"display_name 'hello' not in repo_map keys: {sorted(repo_map)}"
    )
    assert repo_map["hello"] == pkg.repo, (
        f"repo_map['hello'] = {repo_map['hello']!r}, expected {pkg.repo!r}"
    )


# ---------------------------------------------------------------------------
# SP5 — SetupAdapter over each SETUPS name is behaviour-equivalent to calling
# the legacy SETUPS function directly.
#
# Parallel-isolation (SP7): SetupAdapter.provision() passes a unique per-call
# UUID prefix (e.g. ``t_a1b2c3d4_``) to the underlying SETUPS function.
# Both the adapter provision call AND the reference call below use distinct
# unique prefixes, so concurrent xdist workers never push to the same
# repo+tag.  No xdist_group serialization is needed — unique prefixes make
# all provisions parallel-safe by construction.
# ---------------------------------------------------------------------------

_ALL_SETUP_NAMES = list(SETUPS.keys())


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
@pytest.mark.parametrize("setup_name", _ALL_SETUP_NAMES)
def test_sp5_setup_adapter_package_keys_match_legacy_setups(
    setup_name: str,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """SP5: SetupAdapter.packages keys after provision() match the display-name
    keys returned by the corresponding legacy SETUPS function.

    The SetupAdapter (via SP7) uses a unique per-provision prefix so concurrent
    xdist workers do not collide on fixed repo names.  This test verifies that
    the adapter's packages dict is keyed by the same display names as the legacy
    function returns — the prefix affects only actual repo names, not display
    names.  The reference call also uses a unique prefix (distinct from the
    adapter's prefix) so it is safe to run in parallel.
    """
    from src.state_providers import SetupAdapter, resolve_state

    provider = resolve_state(f"setup:{setup_name}")
    assert isinstance(provider, SetupAdapter), (
        f"resolve_state('setup:{setup_name}') returned {type(provider).__name__}"
    )

    adapter_tmp = tmp_path / "adapter"
    adapter_tmp.mkdir()
    provider.provision(ocx, adapter_tmp)

    # Call legacy function directly with a unique prefix (parallel-safe).
    # We only need the display-name keys — the prefix does not affect them.
    ref_prefix = f"t_{uuid4().hex[:8]}_"
    ref_tmp = tmp_path / "ref"
    ref_tmp.mkdir()
    legacy_result = SETUPS[setup_name](ocx, ref_tmp, prefix=ref_prefix)
    expected_display_keys = set(legacy_result.keys())

    assert set(provider.packages.keys()) == expected_display_keys, (
        f"setup:{setup_name} adapter packages keys "
        f"{sorted(provider.packages.keys())} != "
        f"legacy keys {sorted(expected_display_keys)}"
    )

    # Each adapter package value must be a PackageInfo
    for display_name, pkg in provider.packages.items():
        assert isinstance(pkg, PackageInfo), (
            f"setup:{setup_name} packages[{display_name!r}] is {type(pkg)}, "
            f"expected PackageInfo"
        )


# ---------------------------------------------------------------------------
# SP6 — ScenarioAdapter over each SCENARIOS key exposes the same $PKG_*
# projection as instantiating the subclass directly + setup() + script_env().
# ---------------------------------------------------------------------------

_ALL_SCENARIO_NAMES = list(SCENARIOS.keys())


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
@pytest.mark.parametrize("scenario_name", _ALL_SCENARIO_NAMES)
def test_sp6_scenario_adapter_pkg_vars_match_direct_instantiation(
    scenario_name: str,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """SP6: ScenarioAdapter.script_env() after provision() exposes the same
    PKG_* variables as calling the Scenario subclass directly.

    The adapter wraps the subclass; the $PKG_* projection must be identical
    (same keys, same value shapes) because both derive from the same package
    dict after setup().
    """
    from src.state_providers import ScenarioAdapter, resolve_state

    # --- Reference: direct instantiation (legacy path) ---
    # Use a separate tmp_path sub-dir so the two repo-name UUID prefixes differ
    # and don't collide in the registry.
    legacy_tmp = tmp_path / "legacy"
    legacy_tmp.mkdir()
    legacy_scenario: Scenario = SCENARIOS[scenario_name](ocx, legacy_tmp)
    legacy_scenario.setup()
    legacy_env = legacy_scenario.script_env()

    legacy_pkg_keys = {k for k in legacy_env if k.startswith("PKG_")}

    # --- Adapter path ---
    adapter_tmp = tmp_path / "adapter"
    adapter_tmp.mkdir()
    provider = resolve_state(f"scenario:{scenario_name}")
    assert isinstance(provider, ScenarioAdapter), (
        f"resolve_state('scenario:{scenario_name}') returned {type(provider).__name__}"
    )
    provider.provision(ocx, adapter_tmp)
    adapter_env = provider.script_env()

    adapter_pkg_keys = {k for k in adapter_env if k.startswith("PKG_")}

    # Same set of PKG_* variable names (display-name derived)
    assert adapter_pkg_keys == legacy_pkg_keys, (
        f"scenario:{scenario_name} — adapter PKG_* keys {sorted(adapter_pkg_keys)} "
        f"!= legacy PKG_* keys {sorted(legacy_pkg_keys)}"
    )

    # All PKG_* values are non-empty strings
    for key in adapter_pkg_keys:
        assert adapter_env[key], (
            f"scenario:{scenario_name} adapter_env[{key!r}] is empty"
        )

    # Runner-level vars present in adapter env too
    for runner_var in ("OCX", "OCX_HOME", "REGISTRY"):
        assert runner_var in adapter_env, (
            f"scenario:{scenario_name} adapter script_env() missing {runner_var!r}"
        )


# ---------------------------------------------------------------------------
# SP7 — SetupAdapter uses unique prefix per provision (parallel-isolation)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp7_two_provisions_of_setup_basic_use_distinct_prefixes(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP7: Two separate SetupAdapter provisions of 'setup:basic' must produce
    distinct ``repo`` / ``fq`` values (unique prefix per provision) but
    identical display-name keys and prefix-agnostic ``$PKG_*`` env projections.

    This asserts the parallel-isolation invariant (design_spec §3 SP7): the
    registry:2 shared instance is not polluted by fixed-repo collisions when
    concurrent xdist workers both provision the same setup family.

    Two *fresh* SetupAdapter instances are used (not the module-level singleton)
    so that the two provisions are genuinely independent and each generates its
    own UUID prefix.
    """
    from recordings.setups import SETUPS

    from src.state_providers import SetupAdapter

    # Two independent provisions — each gets its own tmp subdir.
    tmp_a = tmp_path / "prov_a"
    tmp_b = tmp_path / "prov_b"
    tmp_a.mkdir()
    tmp_b.mkdir()

    provider_a = SetupAdapter("basic", SETUPS["basic"])
    provider_b = SetupAdapter("basic", SETUPS["basic"])

    provider_a.provision(ocx, tmp_a)
    provider_b.provision(ocx, tmp_b)

    # Both must have a "uv" package (the basic setup canonical package).
    assert "uv" in provider_a.packages, (
        "SP7: setup:basic provision A must have a 'uv' package"
    )
    assert "uv" in provider_b.packages, (
        "SP7: setup:basic provision B must have a 'uv' package"
    )

    pkg_a = provider_a.packages["uv"]
    pkg_b = provider_b.packages["uv"]

    # Each provision uses a unique prefix → actual repo names must differ.
    assert pkg_a.repo != pkg_b.repo, (
        f"SP7: two provisions of setup:basic must use distinct repo prefixes; "
        f"both got repo={pkg_a.repo!r}"
    )
    assert pkg_a.fq != pkg_b.fq, (
        f"SP7: two provisions of setup:basic must use distinct fq values; "
        f"both got fq={pkg_a.fq!r}"
    )

    # Display-name dict keys must be identical (prefix-agnostic).
    assert set(provider_a.packages.keys()) == set(provider_b.packages.keys()), (
        f"SP7: display-name keys must be identical across provisions; "
        f"A={sorted(provider_a.packages.keys())}, B={sorted(provider_b.packages.keys())}"
    )

    # $PKG_UV env var format is display-name–derived (prefix-agnostic):
    # its *key* must be present in both envs (same display name).
    env_a = provider_a.script_env()
    env_b = provider_b.script_env()
    assert "PKG_UV" in env_a, "SP7: provision A env missing PKG_UV"
    assert "PKG_UV" in env_b, "SP7: provision B env missing PKG_UV"
    # Values must differ (actual repo name embedded in short/fq value).
    assert env_a["PKG_UV"] != env_b["PKG_UV"], (
        f"SP7: PKG_UV values must differ between provisions; "
        f"both got {env_a['PKG_UV']!r}"
    )


# ---------------------------------------------------------------------------
# SP8 — work_dir / publisher working-directory contract
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp8_setup_adapter_work_dir_is_none_before_provision(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP8: SetupAdapter.work_dir is None before provision() is called.

    Uses a freshly constructed SetupAdapter (not the module-level singleton)
    to guarantee no previous provision has set work_dir on this instance.

    Design ref: §3 SP8 — 'set only after provision() has been called'.
    """
    from recordings.setups import SETUPS

    from src.state_providers import SetupAdapter

    # Fresh instance — never provisioned
    provider = SetupAdapter("basic", SETUPS["basic"])
    assert provider.work_dir is None, (
        f"SP8: SetupAdapter.work_dir must be None before provision(); "
        f"got {provider.work_dir!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp8_setup_adapter_work_dir_is_path_after_provision(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP8: SetupAdapter.work_dir is a Path that exists after provision().

    Uses a freshly constructed SetupAdapter to verify the exact work_dir
    relationship in SetupAdapter.provision():
    ``state_path = tmp_path / "_state"`` and ``self.work_dir = state_path``.

    Design ref: §3 SP8 — 'SetupAdapter.provision calls the function with
    state_path = tmp_path / "_state"'.
    """
    from recordings.setups import SETUPS

    from src.state_providers import SetupAdapter

    provider = SetupAdapter("basic", SETUPS["basic"])

    prov_tmp = tmp_path / "prov"
    prov_tmp.mkdir()
    provider.provision(ocx, prov_tmp)

    assert provider.work_dir is not None, (
        "SP8: SetupAdapter.work_dir must not be None after provision()"
    )
    assert isinstance(provider.work_dir, Path), (
        f"SP8: SetupAdapter.work_dir must be a Path; got {type(provider.work_dir)}"
    )
    assert provider.work_dir.exists(), (
        f"SP8: SetupAdapter.work_dir must exist after provision(); "
        f"got {provider.work_dir!r}"
    )
    # work_dir must be the _state subdirectory of the provision tmp.
    expected_work_dir = prov_tmp / "_state"
    assert provider.work_dir == expected_work_dir, (
        f"SP8: SetupAdapter.work_dir must equal tmp_path / '_state'; "
        f"expected {expected_work_dir!r}, got {provider.work_dir!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_sp8_scenario_adapter_work_dir_is_none_before_and_after_provision(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SP8: ScenarioAdapter.work_dir is None both before AND after provision().

    Uses a freshly constructed ScenarioAdapter.  Scenario adapters do not
    write a publisher-style work tree, so work_dir is always None
    (design_spec §3 SP8: 'ScenarioAdapter always returns None').
    """
    from src.scenarios import SCENARIOS

    from src.state_providers import ScenarioAdapter

    provider = ScenarioAdapter("BasicPackage", SCENARIOS["BasicPackage"])

    # Before provision
    assert provider.work_dir is None, (
        f"SP8: ScenarioAdapter.work_dir must be None before provision(); "
        f"got {provider.work_dir!r}"
    )

    prov_tmp = tmp_path / "prov_scenario"
    prov_tmp.mkdir()
    provider.provision(ocx, prov_tmp)

    # After provision — still None
    assert provider.work_dir is None, (
        f"SP8: ScenarioAdapter.work_dir must remain None after provision(); "
        f"got {provider.work_dir!r}"
    )


# ===========================================================================
# DE0 — Hat-1 characterization oracle (committed first)
#
# Pins the post-provision provider.packages short refs for every SETUPS name
# and every Scenario subclass.  This is the oracle that DE3/DE6 must match.
#
# Expected behaviour NOW (pre-implement): PASSES — provision() works and
# provider.packages is populated; declared_display_env() is NOT called here.
#
# This oracle is the characterization baseline.  Any change to a SETUPS
# function or Scenario.setup() that alters the canonical package short refs
# will red THIS test first, forcing an explicit oracle update before DE3/DE6
# can regress silently.
#
# Design ref: design_spec_doc_command_scripts.md §6f DE0 / §10 index.
# ===========================================================================


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
@pytest.mark.parametrize("setup_name", list(SETUPS.keys()))
def test_de0_oracle_setup_packages_short_refs_post_provision(
    setup_name: str,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """DE0 oracle (setup family): after provision(), provider.packages maps
    display names → PackageInfo with non-empty .short refs.

    This test is the characterization oracle for DE3/DE6.  It does NOT call
    declared_display_env() — it only reads the provisioned packages dict.
    Expected to PASS today (stub does not affect provision).

    The actual short refs contain the SP7 UUID prefix so they are NOT
    pinned to a literal value here — the oracle asserts structural shape
    (dict keyed by display name, each value a PackageInfo with non-empty
    short).  The literal value oracle is enforced by DE6 after the
    DECLARED_PACKAGES table is populated (DE6 compares PKG_<KEY>→pkg.short).
    """
    from src.state_providers import SetupAdapter, resolve_state

    provider = resolve_state(f"setup:{setup_name}")
    assert isinstance(provider, SetupAdapter)

    prov_tmp = tmp_path / f"prov_{setup_name}"
    prov_tmp.mkdir()
    provider.provision(ocx, prov_tmp)

    assert isinstance(provider.packages, dict), (
        f"DE0 oracle: setup:{setup_name}.packages is {type(provider.packages)}, expected dict"
    )
    assert provider.packages, (
        f"DE0 oracle: setup:{setup_name}.packages is empty after provision()"
    )
    for display_name, pkg in provider.packages.items():
        assert isinstance(display_name, str) and display_name, (
            f"DE0 oracle: setup:{setup_name} has empty display_name key"
        )
        assert isinstance(pkg.short, str) and pkg.short, (
            f"DE0 oracle: setup:{setup_name}.packages[{display_name!r}].short is empty"
        )
        # short ref has format  "<repo>:<tag>"
        assert ":" in pkg.short, (
            f"DE0 oracle: setup:{setup_name}.packages[{display_name!r}].short "
            f"{pkg.short!r} missing ':' separator"
        )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
@pytest.mark.parametrize("scenario_name", list(SCENARIOS.keys()))
def test_de0_oracle_scenario_packages_short_refs_post_provision(
    scenario_name: str,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """DE0 oracle (scenario family): after provision(), provider.packages maps
    display names → PackageInfo with non-empty .short refs.

    Same oracle contract as the setup family above but for ScenarioAdapters.
    Expected to PASS today.

    Design ref: design_spec_doc_command_scripts.md §6f DE0.
    """
    from src.state_providers import ScenarioAdapter, resolve_state

    provider = resolve_state(f"scenario:{scenario_name}")
    assert isinstance(provider, ScenarioAdapter)

    prov_tmp = tmp_path / f"prov_{scenario_name}"
    prov_tmp.mkdir()
    provider.provision(ocx, prov_tmp)

    assert isinstance(provider.packages, dict), (
        f"DE0 oracle: scenario:{scenario_name}.packages is {type(provider.packages)}"
    )
    assert provider.packages, (
        f"DE0 oracle: scenario:{scenario_name}.packages is empty after provision()"
    )
    for display_name, pkg in provider.packages.items():
        assert isinstance(display_name, str) and display_name, (
            f"DE0 oracle: scenario:{scenario_name} has empty display_name key"
        )
        assert isinstance(pkg.short, str) and pkg.short, (
            f"DE0 oracle: scenario:{scenario_name}.packages[{display_name!r}].short is empty"
        )
        assert ":" in pkg.short, (
            f"DE0 oracle: scenario:{scenario_name}.packages[{display_name!r}].short "
            f"{pkg.short!r} missing ':' separator"
        )


# ===========================================================================
# DE2 — declared_display_env() is static, zero-I/O
#
# Contract: the accessor looks up the module-level DECLARED_PACKAGES table
# and projects {PKG_<KEY>: <short_ref>} without calling provision() or
# performing any I/O.
#
# Expected behaviour NOW: FAILS — raises NotImplementedError because
# DECLARED_PACKAGES is still empty {}. These tests encode the expected
# post-implement contract (method returns dict[str, str], no exception).
#
# Design ref: design_spec_doc_command_scripts.md §6f DE2.
# ===========================================================================


def test_de2_declared_display_env_returns_dict_without_provision() -> None:
    """DE2: declared_display_env() returns a dict[str, str] without any
    provision() call — static, zero-I/O.

    The returned keys must be in the renderable matrix — PKG_<KEY> (→ short
    ref) or REPO_<KEY> (→ bare repo name) — no $, no FQ_*/TAG_*/MARKER_*
    (LDR 2026-05-17).  Values are canonical (e.g. 'uv:0.10' / 'uv').

    setup:basic declares a 'uv' package → PKG_UV key expected.

    Expected to FAIL now (NotImplementedError until Implement phase).

    Design ref: design_spec_doc_command_scripts.md §6f DE2.
    """
    from src.state_providers import resolve_state

    provider = resolve_state("setup:basic")
    # Must NOT raise; no provision() called
    result = provider.declared_display_env()  # FAILS today: NotImplementedError

    assert isinstance(result, dict), (
        f"DE2: declared_display_env() must return dict; got {type(result)}"
    )
    # All keys must be in the renderable matrix: PKG_<KEY> or REPO_<KEY>
    # (LDR 2026-05-17 — REPO_ is renderable: bare repo name).
    for key in result:
        assert key.startswith(("PKG_", "REPO_")), (
            f"DE2: declared_display_env() key {key!r} not in renderable "
            f"matrix (PKG_<KEY> / REPO_<KEY>)"
        )
    # Values must be non-empty strings. PKG_<KEY> = short ref (`name:tag`,
    # has ':'); REPO_<KEY> = bare repo name (no ':' — LDR 2026-05-17).
    for key, val in result.items():
        assert isinstance(val, str) and val, (
            f"DE2: declared_display_env() value for {key!r} is empty or not str"
        )
        if key.startswith("PKG_"):
            assert ":" in val, (
                f"DE2: PKG_ value for {key!r} lacks ':' — not a short ref: {val!r}"
            )
        else:  # REPO_<KEY> — bare repo, must NOT carry a tag
            assert ":" not in val, (
                f"DE2: REPO_ value for {key!r} must be a bare repo (no ':'): {val!r}"
            )


def test_de2_declared_display_env_is_identical_before_and_after_provision(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """DE2: declared_display_env() returns the SAME result before and after
    provision() — it is a pure static accessor, not computed from provisioned state.

    Expected to FAIL now (NotImplementedError until Implement phase).

    Design ref: design_spec_doc_command_scripts.md §6f DE2.
    """
    from recordings.setups import SETUPS

    from src.state_providers import SetupAdapter

    # Fresh instance — never provisioned
    provider = SetupAdapter("basic", SETUPS["basic"])

    # Before provision
    before = provider.declared_display_env()  # FAILS today: NotImplementedError

    prov_tmp = tmp_path / "prov"
    prov_tmp.mkdir()
    provider.provision(ocx, prov_tmp)

    # After provision — must return identical result
    after = provider.declared_display_env()

    assert before == after, (
        f"DE2: declared_display_env() must be static — result must be identical "
        f"before and after provision().\n  before: {before}\n  after:  {after}"
    )


@pytest.mark.parametrize(
    "state_key",
    [f"setup:{name}" for name in SETUPS]
    + [f"scenario:{name}" for name in SCENARIOS],
)
def test_de2_declared_display_env_returns_dict_for_every_state(
    state_key: str,
) -> None:
    """DE2: declared_display_env() returns a dict[str, str] for EVERY registered
    state key — no exceptions, no wrong types.

    Parametrized over all 7 setup + 6 scenario states (13 total).
    No provision() called — method is static, zero-I/O.

    Expected to FAIL now (NotImplementedError until Implement phase).

    Design ref: design_spec_doc_command_scripts.md §6f DE2.
    """
    from src.state_providers import resolve_state

    provider = resolve_state(state_key)
    result = provider.declared_display_env()  # FAILS today: NotImplementedError

    assert isinstance(result, dict), (
        f"DE2: {state_key} declared_display_env() must return dict; got {type(result)}"
    )
    # {} is valid (provider with no declared packages)
    for key, val in result.items():
        assert key.startswith(("PKG_", "REPO_")), (
            f"DE2: {state_key} declared_display_env() key {key!r} not in "
            f"renderable matrix (PKG_<KEY> / REPO_<KEY>; LDR 2026-05-17)"
        )
        assert isinstance(val, str) and val, (
            f"DE2: {state_key} declared_display_env() value for {key!r} is empty"
        )


def test_de2_declared_packages_module_table_keyed_by_family_qualified_name() -> None:
    """DE2 (Living Design Record): the DECLARED_PACKAGES module-level table is
    keyed by family-qualified state names ('setup:<name>' / 'scenario:<Name>').

    After implementation, the table must:
    - Not be empty (at least one entry for setup:basic)
    - Have only 'setup:' or 'scenario:' prefixed keys
    - Each value must be a dict[str, str] mapping display_key → short_ref

    Expected to FAIL now (DECLARED_PACKAGES is {}).

    Design ref: design_spec_doc_command_scripts.md §6f DE2 LDR shape.
    """
    from src.state_providers import DECLARED_PACKAGES

    # After implementation, the table must not be empty
    assert DECLARED_PACKAGES, (
        "DE2: DECLARED_PACKAGES must be populated after Implement phase; currently empty. "
        "Add entries keyed by 'setup:<name>'/'scenario:<Name>' to state_providers.py."
    )

    for state_key, pkg_map in DECLARED_PACKAGES.items():
        assert state_key.startswith(("setup:", "scenario:")), (
            f"DE2: DECLARED_PACKAGES key {state_key!r} is not family-qualified"
        )
        assert isinstance(pkg_map, dict), (
            f"DE2: DECLARED_PACKAGES[{state_key!r}] must be dict; got {type(pkg_map)}"
        )
        for display_key, short_ref in pkg_map.items():
            assert isinstance(display_key, str) and display_key, (
                f"DE2: DECLARED_PACKAGES[{state_key!r}] has empty display_key"
            )
            assert isinstance(short_ref, str) and short_ref, (
                f"DE2: DECLARED_PACKAGES[{state_key!r}][{display_key!r}] is empty"
            )


# ===========================================================================
# DE3 — declared values must match the DE0 oracle
#
# Contract: for each state, declared_display_env() returns exactly
# {PKG_<KEY>: pkg.short} for the canonical (first) version — same as the
# DE0 oracle's PKG_<KEY>→pkg.short projection from provider.packages.
#
# Expected behaviour NOW: FAILS (NotImplementedError from DE2).
#
# Design ref: design_spec_doc_command_scripts.md §6f DE3.
# ===========================================================================


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
@pytest.mark.parametrize("setup_name", list(SETUPS.keys()))
def test_de3_declared_display_env_matches_oracle_setup(
    setup_name: str,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """DE3: setup family — declared_display_env() values equal the DE0 oracle's
    PKG_<KEY>→pkg.short projection of the post-provision packages dict.

    The oracle (DE0) establishes what the provisioned truth is.  DE3 requires
    the static declared surface (DE2) to agree.

    Expected to FAIL now (NotImplementedError until Implement phase).

    Design ref: design_spec_doc_command_scripts.md §6f DE3.
    """
    from src.state_providers import resolve_state

    provider = resolve_state(f"setup:{setup_name}")
    prov_tmp = tmp_path / f"prov_{setup_name}"
    prov_tmp.mkdir()
    provider.provision(ocx, prov_tmp)

    # Oracle: what the provisioned packages actually are
    oracle: dict[str, str] = _canonical_proj(provider.packages)

    # Declared: what the static accessor claims
    declared = provider.declared_display_env()  # FAILS today: NotImplementedError

    assert declared == oracle, (
        f"DE3: setup:{setup_name} declared_display_env() mismatch with DE0 oracle.\n"
        f"  declared: {declared}\n"
        f"  oracle:   {oracle}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
@pytest.mark.parametrize("scenario_name", list(SCENARIOS.keys()))
def test_de3_declared_display_env_matches_oracle_scenario(
    scenario_name: str,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """DE3: scenario family — declared_display_env() values equal the DE0
    oracle's PKG_<KEY>→pkg.short projection.

    Expected to FAIL now (NotImplementedError until Implement phase).

    Design ref: design_spec_doc_command_scripts.md §6f DE3.
    """
    from src.state_providers import resolve_state

    provider = resolve_state(f"scenario:{scenario_name}")
    prov_tmp = tmp_path / f"prov_{scenario_name}"
    prov_tmp.mkdir()
    provider.provision(ocx, prov_tmp)

    oracle: dict[str, str] = _canonical_proj(provider.packages)

    declared = provider.declared_display_env()  # FAILS today: NotImplementedError

    assert declared == oracle, (
        f"DE3: scenario:{scenario_name} declared_display_env() mismatch with DE0 oracle.\n"
        f"  declared: {declared}\n"
        f"  oracle:   {oracle}"
    )


# ===========================================================================
# DE4 — SP0 invariant extended: zero registry/network I/O
#
# Contract: importing src.state_providers and src.doc_scripts and calling
# declared_display_env() + doc_scripts_export() performs ZERO network I/O —
# no OCI push, no HTTP call, no Docker socket touch.  Runs in a subprocess
# with no registry fixture; mirrors the existing SP0 test technique.
#
# Expected behaviour NOW: FAILS — declared_display_env() raises
# NotImplementedError which leaks as a non-zero subprocess exit.
#
# Design ref: design_spec_doc_command_scripts.md §6f DE4.
# ===========================================================================


def test_de4_declared_display_env_and_export_perform_zero_network_io() -> None:
    """DE4 (SP0 extended): importing state_providers + doc_scripts and calling
    declared_display_env() on every registered provider + running
    doc_scripts_export() performs zero registry / network I/O.

    Technique mirrors test_sp0_import_performs_zero_registry_io: run in a
    child process that has no Docker / registry available; any network call
    would fail or timeout.  The test asserts exit 0 and the presence of
    the sentinel 'OK' in stdout.

    Expected to FAIL now because declared_display_env() raises
    NotImplementedError (the table is empty).

    Design ref: design_spec_doc_command_scripts.md §6f DE4.
    """
    import subprocess

    code = """
import sys
from pathlib import Path

# Resolve test/ root: this script is run from the test/ directory
_TEST_DIR = Path(".").resolve()
if str(_TEST_DIR) not in sys.path:
    sys.path.insert(0, str(_TEST_DIR))

from src import state_providers
from src.doc_scripts import doc_scripts_export

# Call declared_display_env() on every registered provider.
# Must NOT raise, must NOT perform any I/O.
for state_key, provider in state_providers.STATE_PROVIDERS.items():
    env = provider.declared_display_env()
    # {} is acceptable (DE3: no declared packages → empty); dict is required
    assert isinstance(env, dict), (
        f"declared_display_env() for {state_key!r} returned {type(env)}, expected dict"
    )

# Run doc_scripts_export on an empty directory (no scripts to parse)
import tempfile, os
with tempfile.TemporaryDirectory() as d:
    entries = doc_scripts_export(Path(d))
    assert entries == [], f"doc_scripts_export on empty dir returned {entries}"

print("OK")
"""
    result = subprocess.run(
        [sys.executable, "-c", code],
        capture_output=True,
        text=True,
        cwd=str(Path(__file__).parent.parent),  # test/ root on PYTHONPATH
    )
    assert result.returncode == 0, (
        f"DE4 zero-I/O test failed (rc={result.returncode}):\n"
        f"stdout: {result.stdout}\n"
        f"stderr: {result.stderr}"
    )
    assert "OK" in result.stdout, (
        f"DE4: expected 'OK' in subprocess stdout; got: {result.stdout!r}"
    )


# ===========================================================================
# DE6 — Declared-vs-provisioned cross-check (value correctness)
#
# Provisioned test:parallel case (same collection as drift gate) — runs under
# task verify as the mandatory final gate.  No skip / opt-in marker.
#
# Contract: after provision(), for each provider:
#   declared_display_env() == {f"PKG_{k.upper().replace('-','_')}": p.short
#                              for k, p in provider.packages.items()}
#
# Catches DECLARED_PACKAGES value-staleness (e.g. multi-version "first wins"
# ordering facts, versions[0].short) that DE4 (static) cannot catch.
#
# Expected behaviour NOW: FAILS (NotImplementedError from DE2).
#
# Design ref: design_spec_doc_command_scripts.md §6f DE6.
# ===========================================================================


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
@pytest.mark.parametrize("setup_name", list(SETUPS.keys()))
def test_de6_declared_vs_provisioned_cross_check_setup(
    setup_name: str,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """DE6: setup family — after provision(), declared_display_env() keys+values
    must equal the PKG_<KEY>→pkg.short projection of provider.packages.

    This is the mandatory staleness gate.  It catches DECLARED_PACKAGES value
    drift that static tests (DE4) cannot: e.g. multi-version whose canonical
    short ref changes when a new version is prepended to versions[0].

    Provisioned: requires registry (runs in test:parallel with registry:2).
    No skip / opt-in marker — runs under task verify unconditionally.

    Expected to FAIL now (NotImplementedError until Implement phase).

    Design ref: design_spec_doc_command_scripts.md §6f DE6.
    """
    from src.state_providers import resolve_state

    provider = resolve_state(f"setup:{setup_name}")
    prov_tmp = tmp_path / f"de6_setup_{setup_name}"
    prov_tmp.mkdir()
    provider.provision(ocx, prov_tmp)

    expected: dict[str, str] = _canonical_proj(provider.packages)

    actual = provider.declared_display_env()  # FAILS today: NotImplementedError

    assert actual == expected, (
        f"DE6: setup:{setup_name} declared_display_env() does not match "
        f"provisioned packages.\n"
        f"  declared (actual):  {actual}\n"
        f"  provisioned (expected): {expected}\n"
        f"Check DECLARED_PACKAGES['setup:{setup_name}'] in src/state_providers.py."
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
@pytest.mark.parametrize("scenario_name", list(SCENARIOS.keys()))
def test_de6_declared_vs_provisioned_cross_check_scenario(
    scenario_name: str,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """DE6: scenario family — after provision(), declared_display_env() keys+values
    must equal the PKG_<KEY>→pkg.short projection of provider.packages.

    Same contract as the setup family.  Provisioned; no skip/opt-in marker.

    Expected to FAIL now (NotImplementedError until Implement phase).

    Design ref: design_spec_doc_command_scripts.md §6f DE6.
    """
    from src.state_providers import resolve_state

    provider = resolve_state(f"scenario:{scenario_name}")
    prov_tmp = tmp_path / f"de6_scenario_{scenario_name}"
    prov_tmp.mkdir()
    provider.provision(ocx, prov_tmp)

    expected: dict[str, str] = _canonical_proj(provider.packages)

    actual = provider.declared_display_env()  # FAILS today: NotImplementedError

    assert actual == expected, (
        f"DE6: scenario:{scenario_name} declared_display_env() does not match "
        f"provisioned packages.\n"
        f"  declared (actual):  {actual}\n"
        f"  provisioned (expected): {expected}\n"
        f"Check DECLARED_PACKAGES['scenario:{scenario_name}'] in src/state_providers.py."
    )
