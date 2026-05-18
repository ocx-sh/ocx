"""Characterization tests for legacy SETUPS and SCENARIOS registries.

These tests are the safety net for the StateProvider / SetupAdapter /
ScenarioAdapter refactor (Phase 1 of plan_tested-doc-commands).  They lock
the current observable behaviour of every legacy name so that the adapters
can be validated against them.

Coverage matrix
---------------

SETUPS (recordings/setups.py):
  basic         — exercised by recordings suite (exec.sh, install.sh, ...)
  multi-version — exercised by recordings suite (env.sh, install-select.sh, ...)
  full-catalog  — exercised by recordings suite (env-multi.sh, index.sh, ...)
  variants      — exercised by recordings suite (variants.sh)
  dependencies  — UNCOVERED by any test suite → characterised here
  deps-visibility — exercised by recordings suite (deps.sh, deps-flat.sh, ...)
  publisher     — exercised by recordings suite (package-create.sh, ...)

SCENARIOS (src/scenarios/__init__.py):
  BasicPackage     — exercised by test_scenarios_smoke (smoke/, offline/ scripts)
  TwoLevelDeps     — exercised by test_scenarios_smoke (transitive-dep-env.sh, ...)
  ThreeLevelDeps   — exercised by test_scenarios_smoke (three-level-dep-env.sh)
  DiamondDeps      — exercised by test_scenarios_smoke (diamond-dep-env.sh)
  MultiEntrypoints — exercised by test_scenarios_smoke (multi-entrypoint.sh)
  MultiLayer       — exercised by test_scenarios_smoke (multi-layer-exec.sh, ...)

Only `dependencies` (SETUPS) is uncovered by the existing suites.
All six Scenario subclasses are covered via test_scenarios_smoke.

This file characterises:
1. The SETUPS registry keys (all 7 must be present).
2. The SCENARIOS registry keys (all 6 must be present).
3. The `dependencies` setup function — observable shape of the returned dict
   (package-dict keys + basic PackageInfo structure).
4. Each Scenario subclass's `self.packages` keys after `setup()`, asserted
   through a real registry (no mocks) to catch API-shape regressions.
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

from recordings.setups import SETUPS
from src.runner import OcxRunner
from src.scenarios import SCENARIOS, Scenario

# ---------------------------------------------------------------------------
# Registry completeness — static, no fixtures needed
# ---------------------------------------------------------------------------


def test_setups_registry_has_all_expected_keys() -> None:
    """SETUPS must contain the 7 canonical setup names."""
    expected = {"basic", "multi-version", "full-catalog", "variants", "dependencies", "deps-visibility", "publisher"}
    assert set(SETUPS.keys()) == expected, (
        f"SETUPS keys changed — adapter contract broken.\n"
        f"  expected: {sorted(expected)}\n"
        f"  actual:   {sorted(SETUPS.keys())}"
    )


def test_scenarios_registry_has_all_expected_keys() -> None:
    """SCENARIOS must contain the 6 canonical scenario names."""
    expected = {"BasicPackage", "TwoLevelDeps", "ThreeLevelDeps", "DiamondDeps", "MultiEntrypoints", "MultiLayer"}
    assert set(SCENARIOS.keys()) == expected, (
        f"SCENARIOS keys changed — adapter contract broken.\n"
        f"  expected: {sorted(expected)}\n"
        f"  actual:   {sorted(SCENARIOS.keys())}"
    )


def test_setups_values_are_callable() -> None:
    """Every SETUPS value must be callable (accepts ocx, tmp_path, prefix)."""
    for name, fn in SETUPS.items():
        assert callable(fn), f"SETUPS[{name!r}] is not callable"


def test_scenarios_values_are_scenario_subclasses() -> None:
    """Every SCENARIOS value must be a subclass of Scenario."""
    for name, cls in SCENARIOS.items():
        assert issubclass(cls, Scenario), f"SCENARIOS[{name!r}] is not a Scenario subclass"
        assert cls.name == name, f"SCENARIOS[{name!r}].name mismatch: got {cls.name!r}"


# ---------------------------------------------------------------------------
# `dependencies` setup — UNCOVERED by existing suites; characterised here.
#
# The legacy `dependencies` SETUPS function pushes to FIXED repo names
# (nodejs/bun/webapp, no per-test UUID isolation — unlike Scenario fixtures).
# Running it concurrently across xdist workers collides on the same registry
# repo+tag (registry:2 → HTTP 500). This is a pre-existing legacy property,
# not changed here (Two Hats). The whole `dependencies` shape is therefore
# locked in ONE test (setup runs once, many asserts — DAMP) so the safety
# net never invokes the fixed-repo cascade push more than once in parallel.
# ---------------------------------------------------------------------------


def test_dependencies_setup_characterization(ocx: OcxRunner, tmp_path: Path) -> None:
    """Lock the full observable shape of the `dependencies` SETUPS function.

    Single test (one cascade push) deliberately: see module note above.
    """
    from src.runner import PackageInfo

    result = SETUPS["dependencies"](ocx, tmp_path, prefix="")
    registry = ocx.registry

    # Dict keys.
    assert set(result.keys()) == {"nodejs", "bun", "webapp"}, (
        f"dependencies setup dict keys changed: {sorted(result.keys())}"
    )

    for key, packages in result.items():
        # Each value is a non-empty list[PackageInfo] with exactly one version.
        assert isinstance(packages, list), f"dependencies[{key!r}] is not a list"
        assert len(packages) == 1, f"dependencies[{key!r}] expected 1 package, got {len(packages)}"
        pkg = packages[0]
        assert isinstance(pkg, PackageInfo), (
            f"dependencies[{key!r}] element is {type(pkg).__name__}, expected PackageInfo"
        )
        # All PackageInfo fields populated.
        assert pkg.repo, f"dependencies[{key!r}].repo is empty"
        assert pkg.tag, f"dependencies[{key!r}].tag is empty"
        assert pkg.short, f"dependencies[{key!r}].short is empty"
        assert pkg.fq, f"dependencies[{key!r}].fq is empty"
        assert pkg.marker, f"dependencies[{key!r}].marker is empty"
        # Same registry for all three (webapp depends on nodejs + bun).
        assert pkg.fq.startswith(registry), (
            f"dependencies[{key!r}].fq={pkg.fq!r} does not start with registry {registry!r}"
        )


# ---------------------------------------------------------------------------
# Scenario subclass shapes — characterise packages dict keys after setup().
# These complement the test_scenarios_smoke.py shell-script runs; they assert
# the Python-observable state the adapters will rely on.
# ---------------------------------------------------------------------------


pytestmark_scenarios = pytest.mark.skipif(
    sys.platform == "win32",
    reason="Scenario setup calls make_package which uses bash scripts — Linux/macOS only.",
)


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_basic_package_scenario_packages_keys(ocx: OcxRunner, tmp_path: Path) -> None:
    """BasicPackage.setup() must populate self.packages with key 'hello'."""
    scenario = SCENARIOS["BasicPackage"](ocx, tmp_path)
    scenario.setup()
    assert set(scenario.packages.keys()) == {"hello"}, (
        f"BasicPackage.packages keys: {sorted(scenario.packages.keys())}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_two_level_deps_scenario_packages_keys(ocx: OcxRunner, tmp_path: Path) -> None:
    """TwoLevelDeps.setup() must populate self.packages with keys 'leaf' and 'app'."""
    scenario = SCENARIOS["TwoLevelDeps"](ocx, tmp_path)
    scenario.setup()
    assert set(scenario.packages.keys()) == {"leaf", "app"}, (
        f"TwoLevelDeps.packages keys: {sorted(scenario.packages.keys())}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_three_level_deps_scenario_packages_keys(ocx: OcxRunner, tmp_path: Path) -> None:
    """ThreeLevelDeps.setup() must populate self.packages with keys 'leaf', 'mid', 'app'."""
    scenario = SCENARIOS["ThreeLevelDeps"](ocx, tmp_path)
    scenario.setup()
    assert set(scenario.packages.keys()) == {"leaf", "mid", "app"}, (
        f"ThreeLevelDeps.packages keys: {sorted(scenario.packages.keys())}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_diamond_deps_scenario_packages_keys(ocx: OcxRunner, tmp_path: Path) -> None:
    """DiamondDeps.setup() must populate self.packages with keys 'leaf', 'left', 'right', 'app'."""
    scenario = SCENARIOS["DiamondDeps"](ocx, tmp_path)
    scenario.setup()
    assert set(scenario.packages.keys()) == {"leaf", "left", "right", "app"}, (
        f"DiamondDeps.packages keys: {sorted(scenario.packages.keys())}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_multi_entrypoints_scenario_packages_keys(ocx: OcxRunner, tmp_path: Path) -> None:
    """MultiEntrypoints.setup() must populate self.packages with key 'toolkit'."""
    scenario = SCENARIOS["MultiEntrypoints"](ocx, tmp_path)
    scenario.setup()
    assert set(scenario.packages.keys()) == {"toolkit"}, (
        f"MultiEntrypoints.packages keys: {sorted(scenario.packages.keys())}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_multi_layer_scenario_packages_keys(ocx: OcxRunner, tmp_path: Path) -> None:
    """MultiLayer.setup() must populate self.packages with key 'pkg'."""
    scenario = SCENARIOS["MultiLayer"](ocx, tmp_path)
    scenario.setup()
    assert set(scenario.packages.keys()) == {"pkg"}, (
        f"MultiLayer.packages keys: {sorted(scenario.packages.keys())}"
    )


# ---------------------------------------------------------------------------
# Scenario script_env projection — characterise $PKG_*, $REPO_*, $TAG_*
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_basic_package_scenario_script_env_vars(ocx: OcxRunner, tmp_path: Path) -> None:
    """BasicPackage script_env() must expose PKG_HELLO, FQ_HELLO, REPO_HELLO, TAG_HELLO, MARKER_HELLO."""
    scenario = SCENARIOS["BasicPackage"](ocx, tmp_path)
    scenario.setup()
    env = scenario.script_env()
    for var in ("PKG_HELLO", "FQ_HELLO", "REPO_HELLO", "TAG_HELLO", "MARKER_HELLO"):
        assert var in env, f"BasicPackage script_env missing {var!r}"
        assert env[var], f"BasicPackage script_env[{var!r}] is empty"


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_two_level_deps_scenario_script_env_vars(ocx: OcxRunner, tmp_path: Path) -> None:
    """TwoLevelDeps script_env() must expose PKG_LEAF, PKG_APP and corresponding projection vars."""
    scenario = SCENARIOS["TwoLevelDeps"](ocx, tmp_path)
    scenario.setup()
    env = scenario.script_env()
    for key in ("LEAF", "APP"):
        for prefix in ("PKG_", "FQ_", "REPO_", "TAG_", "MARKER_"):
            var = f"{prefix}{key}"
            assert var in env, f"TwoLevelDeps script_env missing {var!r}"
            assert env[var], f"TwoLevelDeps script_env[{var!r}] is empty"


@pytest.mark.skipif(sys.platform == "win32", reason="bash required")
def test_scenario_script_env_always_includes_ocx_home(ocx: OcxRunner, tmp_path: Path) -> None:
    """script_env() must always set OCX_HOME and OCX (binary path)."""
    scenario = SCENARIOS["BasicPackage"](ocx, tmp_path)
    scenario.setup()
    env = scenario.script_env()
    assert "OCX_HOME" in env, "script_env missing OCX_HOME"
    assert "OCX" in env, "script_env missing OCX"
    assert "REGISTRY" in env, "script_env missing REGISTRY"
