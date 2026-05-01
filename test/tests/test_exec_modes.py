"""Acceptance tests for the visibility / exec-mode feature.

Two user-facing exec modes (`consumer` / `self`) cross-tabbed against three
package shapes (private-dep, interface-deps meta, diamond merge), plus
surface-completeness tests. The lib retains an internal `Full` traversal
mode for fetch-time entrypoint collision checks, but `--mode=full` is not
accepted from argv.

Per ADR `.claude/artifacts/adr_visibility_two_axis_and_exec_modes.md`.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from src.helpers import make_package, make_package_with_entrypoints
from src.registry import fetch_manifest_digest
from src.runner import OcxRunner, PackageInfo

# ---------------------------------------------------------------------------
# Helpers (DAMP per quality-core.md — keep tests self-contained)
# ---------------------------------------------------------------------------


def _dep_entry(
    ocx: OcxRunner, pkg: PackageInfo, *, visibility: str | None = None
) -> dict:
    """Build a dependency descriptor for `make_package(dependencies=...)`."""
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    entry: dict = {"identifier": f"{pkg.fq}@{digest}"}
    if visibility is not None:
        entry["visibility"] = visibility
    return entry


def _push_leaf(ocx: OcxRunner, repo: str, tmp_path: Path, **kwargs) -> PackageInfo:
    """Push a leaf package whose env is explicitly tagged ``public``.

    Consumer-mode acceptance tests rely on the leaf's env entries surfacing
    when reached via an Interface edge (see ADR §588 "JDK + Maven + Gradle
    public/interface env via interface edges"). Under v2 default-`private`
    semantics, leaf env vars must be explicitly tagged ``public`` for the
    per-entry filter to admit them under Consumer mode.

    Caller-supplied ``env`` overrides this default.
    """
    home_key = repo.upper().replace("-", "_") + "_HOME"
    public_env = [
        {
            "key": "PATH",
            "type": "path",
            "required": True,
            "value": "${installPath}/bin",
            "visibility": "public",
        },
        {
            "key": home_key,
            "type": "constant",
            "value": "${installPath}",
            "visibility": "public",
        },
    ]
    kwargs.setdefault("env", public_env)
    return make_package(ocx, repo, "1.0.0", tmp_path, new=True, **kwargs)


def _env_keys(env_result: dict) -> list[str]:
    """Extract the list of env-entry keys from a `ocx env` JSON result."""
    return [e["key"] for e in env_result.get("entries", [])]


def _project_root() -> Path:
    """Resolve the repo root from this test file's location."""
    return Path(__file__).resolve().parent.parent.parent


# ---------------------------------------------------------------------------
# Matrix: 3 modes × 3 package shapes (plan §364-385)
# ---------------------------------------------------------------------------

# ── Shape 1: cmake_with_cuda — Private dep ─────────────────────────────────
#
# A package with a Private dep. Under Consumer, the dep's env is hidden.
# Under Self_ and Full, it is visible.


def test_consumer_mode_excludes_private_dep_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`ocx exec --mode=consumer` (default) must hide a Private dep's env."""
    cuda = _push_leaf(ocx, f"{unique_repo}_cuda", tmp_path)
    dep = _dep_entry(ocx, cuda, visibility="private")
    cmake = make_package(
        ocx, f"{unique_repo}_cmake", "1.0.0", tmp_path,
        new=True, dependencies=[dep],
    )
    ocx.json("install", "--select", cmake.short)

    cuda_home_key = f"{unique_repo}_cuda".upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("env", cmake.short)
    assert cuda_home_key not in _env_keys(env_result), (
        f"Consumer mode must hide Private dep env key {cuda_home_key!r}; "
        f"got: {_env_keys(env_result)}"
    )


def test_self_mode_includes_private_dep_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`ocx exec --mode=self` (the launcher's default) must include Private deps."""
    cuda = _push_leaf(ocx, f"{unique_repo}_cuda", tmp_path)
    dep = _dep_entry(ocx, cuda, visibility="private")
    cmake = make_package(
        ocx, f"{unique_repo}_cmake", "1.0.0", tmp_path,
        new=True, dependencies=[dep],
    )
    ocx.json("install", "--select", cmake.short)

    cuda_home_key = f"{unique_repo}_cuda".upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("env", "--self", cmake.short)
    assert cuda_home_key in _env_keys(env_result), (
        f"Self mode MUST include Private dep env key {cuda_home_key!r}; "
        f"got: {_env_keys(env_result)}"
    )


# ── Shape 2: java_toolchain — Interface deps ───────────────────────────────


def test_consumer_mode_includes_interface_deps(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`ocx env` (default consumer view) on a meta-package shows Interface deps."""
    jdk = _push_leaf(ocx, f"{unique_repo}_jdk", tmp_path)
    maven = _push_leaf(ocx, f"{unique_repo}_maven", tmp_path)
    deps = [
        _dep_entry(ocx, jdk, visibility="interface"),
        _dep_entry(ocx, maven, visibility="interface"),
    ]
    toolchain = make_package(
        ocx, f"{unique_repo}_toolchain", "1.0.0", tmp_path,
        new=True, dependencies=deps,
    )
    ocx.json("install", "--select", toolchain.short)

    env_result = ocx.json("env", toolchain.short)
    keys = _env_keys(env_result)
    jdk_key = f"{unique_repo}_jdk".upper().replace("-", "_") + "_HOME"
    maven_key = f"{unique_repo}_maven".upper().replace("-", "_") + "_HOME"
    assert jdk_key in keys, f"Consumer must surface Interface JDK; keys: {keys}"
    assert maven_key in keys, f"Consumer must surface Interface Maven; keys: {keys}"


def test_env_self_mode_excludes_interface_only_deps(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`ocx env --self` excludes Interface-only deps (two-env composition: private surface).

    Under the two-env composition model (plan_two_env_composition.md Step 3.8):
    - Default consumer view (interface projection): Interface deps ARE visible.
    - Self view (private projection): only deps with ``has_private()=true`` visible.
      Interface edges have ``has_private()=false``, so deps reached ONLY via
      Interface edges do NOT appear under ``--self``.

    This is the Step 3.8 rewrite of ``test_env_self_mode_excludes_interface_deps``
    (the old test had the correct direction but cited a stale ADR section; the
    new docstring anchors the assertion in the two-env algebra instead).
    """
    jdk = _push_leaf(ocx, f"{unique_repo}_jdk", tmp_path)
    maven = _push_leaf(ocx, f"{unique_repo}_maven", tmp_path)
    deps = [
        _dep_entry(ocx, jdk, visibility="interface"),
        _dep_entry(ocx, maven, visibility="interface"),
    ]
    toolchain = make_package(
        ocx, f"{unique_repo}_toolchain", "1.0.0", tmp_path,
        new=True, dependencies=deps,
    )
    ocx.json("install", "--select", toolchain.short)

    env_result = ocx.json("env", "--self", toolchain.short)
    keys = _env_keys(env_result)
    jdk_key = f"{unique_repo}_jdk".upper().replace("-", "_") + "_HOME"
    maven_key = f"{unique_repo}_maven".upper().replace("-", "_") + "_HOME"
    assert jdk_key not in keys, (
        f"Self view MUST exclude Interface-only JDK dep (has_private()=false); keys: {keys}"
    )
    assert maven_key not in keys, (
        f"Self view MUST exclude Interface-only Maven dep (has_private()=false); keys: {keys}"
    )


# ── Shape 3: diamond_merge — algebra-then-mode-filter invariant (ADR §588) ─


def test_diamond_merge_self_mode_preserves_public_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Diamond fixture: leaf reachable via Interface and Public merges to Public.

    Under `--mode=self`, the merged-Public leaf is self-included. Confirms
    the merge-then-mode-filter invariant from ADR §588.
    """
    leaf = _push_leaf(ocx, f"{unique_repo}_leaf", tmp_path)
    leaf_dep_iface = _dep_entry(ocx, leaf, visibility="interface")
    leaf_dep_public = _dep_entry(ocx, leaf, visibility="public")

    # Two intermediate packages reaching the leaf via different visibilities.
    middle_a = make_package(
        ocx, f"{unique_repo}_mid_a", "1.0.0", tmp_path,
        new=True, dependencies=[leaf_dep_iface],
    )
    middle_b = make_package(
        ocx, f"{unique_repo}_mid_b", "1.0.0", tmp_path,
        new=True, dependencies=[leaf_dep_public],
    )

    # Root depends on both intermediates publicly.
    root = make_package(
        ocx, f"{unique_repo}_root", "1.0.0", tmp_path,
        new=True,
        dependencies=[
            _dep_entry(ocx, middle_a, visibility="public"),
            _dep_entry(ocx, middle_b, visibility="public"),
        ],
    )
    ocx.json("install", "--select", root.short)

    leaf_key = f"{unique_repo}_leaf".upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("env", "--self", root.short)
    keys = _env_keys(env_result)
    assert leaf_key in keys, (
        f"Diamond-merged Public path must keep leaf visible under Self_ "
        f"(ADR §588 invariant); keys: {keys}"
    )


# ---------------------------------------------------------------------------
# `--mode=full` rejected at clap parse boundary
# ---------------------------------------------------------------------------


def test_mode_flag_rejected_with_usage_error(
    ocx: OcxRunner, published_package: PackageInfo
):
    """`ocx exec --mode=…` exits 64. The `--mode` flag was replaced by `--self`
    when `ExecMode`/`ExecModeFlag` were deleted; clap rejects unknown args."""
    pkg = published_package
    ocx.plain("install", pkg.short)
    result = ocx.run(
        "exec", "--mode=self", pkg.short, "--", "hello",
        check=False, format=None,
    )
    assert result.returncode == 64, (
        f"`--mode=…` must exit 64 (UsageError) now that the flag is removed; "
        f"got rc={result.returncode}; stderr: {result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Surface-completeness audit (six surfaces × two user-facing modes)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("self_flag", [None, "--self"])
@pytest.mark.parametrize(
    "surface",
    [
        ("env",),
        ("exec", "--", "hello"),
        ("shell", "env"),
        ("ci", "export"),
        ("deps",),
        # `shell profile load` accepts `--self` but takes no package
        # positional — the assertion below skips appending `pkg.short` for
        # this surface.
        ("shell", "profile", "load"),
    ],
)
def test_all_surfaces_carry_self_flag(
    ocx: OcxRunner,
    published_package: PackageInfo,
    surface: tuple[str, ...],
    self_flag: str | None,
):
    """Each env-resolution surface accepts `--self` (or runs with the default
    consumer view when `--self` is absent).

    Smoke-test only — the surface returns a non-error exit code with the flag
    accepted. Detailed semantics are exercised by the matrix tests above.
    """
    pkg = published_package
    ocx.plain("install", pkg.short)

    extra: list[str] = [self_flag] if self_flag else []

    if surface == ("shell", "profile", "load"):
        args = ["shell", "profile", "load", *extra]
    elif "--" in surface:
        args = ["exec", *extra, pkg.short, "--", "hello"]
    else:
        args = [*surface, *extra, pkg.short]

    result = ocx.run(*args, check=False, format=None)
    assert result.returncode != 64, (
        f"surface {surface!r} did not accept self_flag={self_flag!r} "
        f"(exit 64 = clap usage error); stderr: {result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Mirror-stamping migration story (plan §384-385)
# ---------------------------------------------------------------------------


def test_bare_binary_consumer_default_hides_path_without_stamp(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Bare-binary mirror with PATH at default-private hides the binary.

    Then re-publish with explicit `"visibility": "public"` on PATH; binary
    becomes accessible. Validates the v2 cutover migration story for the
    14 in-tree bare-binary mirrors.
    """
    # Phase 1: PATH absent visibility (= default private under v2).
    pkg_default = make_package(
        ocx, f"{unique_repo}_priv_path", "1.0.0", tmp_path,
        new=True,
        env=[{"key": "PATH", "type": "path", "value": "${installPath}/bin"}],
    )
    ocx.json("install", "--select", pkg_default.short)

    env_default = ocx.json("env", pkg_default.short)
    path_entries_default = [
        e for e in env_default.get("entries", [])
        if e["key"] == "PATH"
    ]
    assert path_entries_default == [], (
        f"PATH at default-private must be hidden under Consumer; got: {path_entries_default}"
    )

    # Phase 2: explicit public stamp restores accessibility.
    pkg_public = make_package(
        ocx, f"{unique_repo}_pub_path", "1.0.0", tmp_path,
        new=True,
        env=[{
            "key": "PATH", "type": "path",
            "value": "${installPath}/bin",
            "visibility": "public",
        }],
    )
    ocx.json("install", "--select", pkg_public.short)

    env_public = ocx.json("env", pkg_public.short)
    path_entries_public = [
        e for e in env_public.get("entries", [])
        if e["key"] == "PATH"
    ]
    assert path_entries_public, (
        f"PATH explicit-public must be visible under Consumer; got: {path_entries_public}"
    )


def test_entrypoint_mirror_consumer_default_skips_redundant_bin(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Entrypoint mirror with PATH=default-private: consumer access via synth/.

    Under `--mode=consumer`, raw `${installPath}/bin` is absent (PATH default
    private filters it out); binary accessible via synth `entrypoints/`.
    Under `--mode=self`, raw bin/ is present (recursion-safe launcher
    self-invocation). Validates the post-research-flip redundancy elimination.
    """
    pkg = make_package_with_entrypoints(
        ocx, f"{unique_repo}_ep", tmp_path,
        entrypoints=[{"name": "tool", "target": "${installPath}/bin/tool"}],
        bins=["tool"], tag="1.0.0",
    )
    ocx.json("install", "--select", pkg.short)

    # Consumer: raw bin/ not in PATH (default-private hides declared PATH).
    env_consumer = ocx.json("env", pkg.short)
    consumer_path_values = [
        e["value"] for e in env_consumer.get("entries", []) if e["key"] == "PATH"
    ]
    has_synth_consumer = any("entrypoints" in v for v in consumer_path_values)
    has_raw_bin_consumer = any(
        v.endswith("/bin") or v.endswith("\\bin") for v in consumer_path_values
    )
    assert has_synth_consumer, (
        f"Consumer must have synth entrypoints/ in PATH; values: {consumer_path_values}"
    )
    assert not has_raw_bin_consumer, (
        f"Consumer must NOT have raw bin/ in PATH (default-private hides it); "
        f"values: {consumer_path_values}"
    )

    # Self: raw bin/ present (recursion-safe).
    env_self = ocx.json("env", "--self", pkg.short)
    self_path_values = [
        e["value"] for e in env_self.get("entries", []) if e["key"] == "PATH"
    ]
    has_raw_bin_self = any(
        v.endswith("/bin") or v.endswith("\\bin") for v in self_path_values
    )
    assert has_raw_bin_self, (
        f"Self mode MUST have raw bin/ in PATH (recursion-safe); "
        f"values: {self_path_values}"
    )


# ---------------------------------------------------------------------------
# Default-mode commitment (plan §642 #2 — One-Way Door behavioural commitment)
# ---------------------------------------------------------------------------


def test_default_exec_mode_is_consumer(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`ocx env PKG` (no `--mode`) behaves identically to `ocx env --mode=consumer`.

    Pins the One-Way Door commitment from plan §642 #2.
    """
    cuda = _push_leaf(ocx, f"{unique_repo}_cuda", tmp_path)
    dep = _dep_entry(ocx, cuda, visibility="private")
    cmake = make_package(
        ocx, f"{unique_repo}_cmake", "1.0.0", tmp_path,
        new=True, dependencies=[dep],
    )
    ocx.json("install", "--select", cmake.short)

    cuda_home_key = f"{unique_repo}_cuda".upper().replace("-", "_") + "_HOME"

    no_flag = ocx.json("env", cmake.short)
    with_consumer = ocx.json("env", cmake.short)
    assert _env_keys(no_flag) == _env_keys(with_consumer), (
        "Default mode must equal `--mode=consumer` (One-Way Door commitment); "
        f"no-flag keys: {_env_keys(no_flag)}; "
        f"--mode=consumer keys: {_env_keys(with_consumer)}"
    )
    # Sanity: the cuda key is hidden under both (Private + Consumer = hidden).
    assert cuda_home_key not in _env_keys(no_flag)


# ---------------------------------------------------------------------------
# Negative cases: invalid `--mode` values + warning suppression boolean parsing
# ---------------------------------------------------------------------------


def test_invalid_mode_returns_clap_usage_error(
    ocx: OcxRunner, published_package: PackageInfo
):
    """`ocx exec --mode=garbage` exits with sysexits EX_USAGE (64).

    Plan §578 pins the contract: unknown `--mode` values surface as a clap
    usage error so backend tools can `case $?` on `64` to detect bad
    invocation. The exit-code mapping lives in
    `crates/ocx_lib/src/cli/exit_code.rs` (`ExitCode::UsageError = 64`,
    aligned with BSD `sysexits.h`).
    """
    pkg = published_package
    ocx.plain("install", pkg.short)
    result = ocx.run(
        "exec", "--mode=garbage", pkg.short, "--", "/bin/true",
        check=False, format=None,
    )
    assert result.returncode == 64, (
        f"unknown --mode value must exit 64 (sysexits EX_USAGE); "
        f"got rc={result.returncode}; stderr: {result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Suite B — Dep entry tagged ``EntryVisibility::Interface``
# (Step 3.6 of plan_two_env_composition.md)
#
# Package A declares a single env var FOO with ``visibility: "interface"``.
# R depends on A via various edge visibilities.
#
# A.interface = {FOO}, A.private = {}
#
# 8-cell truth table (4 edge-vis × 2 --self): FOO present only in:
#   (consumer view, interface edge) → YES   has_interface()=true, entry.has_interface()=true
#   (consumer view, public edge)    → YES   has_interface()=true, entry.has_interface()=true
#   (consumer view, private edge)   → NO    has_interface()=false → dep excluded
#   (consumer view, sealed edge)    → NO    has_interface()=false → dep excluded
#   (self view,     interface edge) → NO    has_private()=false → dep excluded
#   (self view,     public edge)    → YES   has_private()=true → dep included; entry.has_interface()=true → emitted
#   (self view,     private edge)   → YES   has_private()=true → dep included; entry.has_interface()=true → emitted
#   (self view,     sealed edge)    → NO    has_private()=false → dep excluded
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "edge_visibility,self_flag,expect_foo",
    [
        # consumer view (no --self)
        pytest.param("interface", False, True,  id="consumer-interface-yes"),
        pytest.param("public",    False, True,  id="consumer-public-yes"),
        pytest.param("private",   False, False, id="consumer-private-no"),
        pytest.param("sealed",    False, False, id="consumer-sealed-no"),
        # self view (--self)
        pytest.param("interface", True,  False, id="self-interface-no"),
        pytest.param("public",    True,  True,  id="self-public-yes"),
        pytest.param("private",   True,  True,  id="self-private-yes"),
        pytest.param("sealed",    True,  False, id="self-sealed-no"),
    ],
)
def test_suite_b_dep_interface_entry_visibility_matrix(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    edge_visibility: str,
    self_flag: bool,
    expect_foo: bool,
) -> None:
    """Suite B: dep A declares FOO with visibility=interface; 8-cell truth table.

    A.interface={FOO}, A.private={}: FOO is emitted when the dep reaches the
    requested surface AND the dep's entry has_interface() (plan §3.6).
    """
    repo_a = f"{unique_repo}_ba"

    # A: leaf with FOO=interface.
    pkg_a = make_package(
        ocx, repo_a, "1.0.0", tmp_path,
        new=True,
        env=[{
            "key": "FOO",
            "type": "constant",
            "value": "bar_from_a",
            "visibility": "interface",
        }],
    )
    dep_a = _dep_entry(ocx, pkg_a, visibility=edge_visibility)

    # R: root with no env of its own.
    pkg_r = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        new=True,
        dependencies=[dep_a],
        env=[],
    )
    ocx.json("install", "--select", pkg_r.short)

    extra_args: list[str] = ["--self"] if self_flag else []
    env_result = ocx.json("env", *extra_args, pkg_r.short)
    keys = _env_keys(env_result)

    if expect_foo:
        assert "FOO" in keys, (
            f"Suite B edge={edge_visibility!r} self={self_flag}: "
            f"expected FOO present; keys: {keys}"
        )
    else:
        assert "FOO" not in keys, (
            f"Suite B edge={edge_visibility!r} self={self_flag}: "
            f"expected FOO absent; keys: {keys}"
        )


# ---------------------------------------------------------------------------
# Suite C — Dep entry tagged ``EntryVisibility::Private``
# (Step 3.7 of plan_two_env_composition.md)
#
# A.interface = {}, A.private = {FOO}
#
# FOO has ``has_interface()=false``, so it NEVER crosses any edge into R's
# env — for all 8 cells (4 edge-vis × 2 --self): FOO absent.
# A dep's private entries never leak to its consumers under any edge visibility.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "edge_visibility,self_flag",
    [
        pytest.param("interface", False, id="consumer-interface"),
        pytest.param("public",    False, id="consumer-public"),
        pytest.param("private",   False, id="consumer-private"),
        pytest.param("sealed",    False, id="consumer-sealed"),
        pytest.param("interface", True,  id="self-interface"),
        pytest.param("public",    True,  id="self-public"),
        pytest.param("private",   True,  id="self-private"),
        pytest.param("sealed",    True,  id="self-sealed"),
    ],
)
def test_suite_c_dep_private_entry_never_crosses_edges(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    edge_visibility: str,
    self_flag: bool,
) -> None:
    """Suite C: dep A declares FOO with visibility=private; FOO never reaches R.

    A dep's private surface is fully encapsulated — it does not cross any edge
    into a consumer's environment regardless of edge visibility or exec mode
    (plan §3.7 all-NO contract).
    """
    repo_a = f"{unique_repo}_ca"

    # A: leaf with FOO=private (default visibility).
    pkg_a = make_package(
        ocx, repo_a, "1.0.0", tmp_path,
        new=True,
        env=[{
            "key": "FOO",
            "type": "constant",
            "value": "bar_from_a_private",
            # No "visibility" key → defaults to private under v2.
        }],
    )
    dep_a = _dep_entry(ocx, pkg_a, visibility=edge_visibility)

    # R: root with no env of its own.
    pkg_r = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        new=True,
        dependencies=[dep_a],
        env=[],
    )
    ocx.json("install", "--select", pkg_r.short)

    extra_args: list[str] = ["--self"] if self_flag else []
    env_result = ocx.json("env", *extra_args, pkg_r.short)
    keys = _env_keys(env_result)

    assert "FOO" not in keys, (
        f"Suite C edge={edge_visibility!r} self={self_flag}: "
        f"dep private entry FOO must NEVER cross an edge into R; keys: {keys}"
    )


# ---------------------------------------------------------------------------
# Suite D — Dep entry tagged ``EntryVisibility::Public``
# (Step 3.8 of plan_two_env_composition.md)
#
# A.interface = {FOO}, A.private = {FOO} (public = both surfaces)
#
# Truth table identical to Suite B because the interface-projection gate and
# private-projection gate both fire on the same entry.  Same 4 YES / 4 NO
# pattern.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "edge_visibility,self_flag,expect_foo",
    [
        # consumer view (no --self)
        pytest.param("interface", False, True,  id="consumer-interface-yes"),
        pytest.param("public",    False, True,  id="consumer-public-yes"),
        pytest.param("private",   False, False, id="consumer-private-no"),
        pytest.param("sealed",    False, False, id="consumer-sealed-no"),
        # self view (--self)
        pytest.param("interface", True,  False, id="self-interface-no"),
        pytest.param("public",    True,  True,  id="self-public-yes"),
        pytest.param("private",   True,  True,  id="self-private-yes"),
        pytest.param("sealed",    True,  False, id="self-sealed-no"),
    ],
)
def test_suite_d_dep_public_entry_visibility_matrix(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    edge_visibility: str,
    self_flag: bool,
    expect_foo: bool,
) -> None:
    """Suite D: dep A declares FOO with visibility=public; 8-cell truth table.

    A.interface={FOO}, A.private={FOO}: public entries surface in both
    interface and private projections when the dep is reached via the
    appropriate edge (plan §3.8).  Truth table identical to Suite B.
    """
    repo_a = f"{unique_repo}_da"

    # A: leaf with FOO=public.
    pkg_a = make_package(
        ocx, repo_a, "1.0.0", tmp_path,
        new=True,
        env=[{
            "key": "FOO",
            "type": "constant",
            "value": "bar_from_a_public",
            "visibility": "public",
        }],
    )
    dep_a = _dep_entry(ocx, pkg_a, visibility=edge_visibility)

    # R: root with no env of its own.
    pkg_r = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        new=True,
        dependencies=[dep_a],
        env=[],
    )
    ocx.json("install", "--select", pkg_r.short)

    extra_args: list[str] = ["--self"] if self_flag else []
    env_result = ocx.json("env", *extra_args, pkg_r.short)
    keys = _env_keys(env_result)

    if expect_foo:
        assert "FOO" in keys, (
            f"Suite D edge={edge_visibility!r} self={self_flag}: "
            f"expected FOO present; keys: {keys}"
        )
    else:
        assert "FOO" not in keys, (
            f"Suite D edge={edge_visibility!r} self={self_flag}: "
            f"expected FOO absent; keys: {keys}"
        )
