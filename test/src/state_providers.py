"""State-provider registry unification.

Replaces the two legacy registries (`recordings.setups.SETUPS` and
`src.scenarios.SCENARIOS`) with a single named registry of `StateProvider`
objects, one per family-qualified state key (``setup:<name>`` /
``scenario:<Name>``).

Design contract reference: design_spec_doc_command_scripts.md §3 (SP0–SP6).

Import-time guarantee (SP0): importing this module performs **zero** registry
I/O.  No OCI push, no network call, no Docker socket touch.  Provisioning
occurs only when `StateProvider.provision()` is called with live fixtures.
"""
from __future__ import annotations

import os
from pathlib import Path
from typing import Protocol
from uuid import uuid4

from src.runner import OcxRunner, PackageInfo


# ---------------------------------------------------------------------------
# Protocol
# ---------------------------------------------------------------------------


class StateProvider(Protocol):
    """Named, registered pre-publish registry state.

    Contract (design_spec §3):

    - SP0: construction / registration performs zero I/O.
    - SP2: ``packages`` is keyed by **display name** (the human-readable name
      used in cast sanitisation — not the UUID-prefixed actual repo name).
    - SP3: ``script_env()`` returns the full Scenario env projection
      (``$PKG_*``, ``$FQ_*``, ``$REPO_*``, ``$TAG_*``, ``$MARKER_*``,
      ``$HOME_KEY_*`` per package; plus ``$OCX``, ``$OCX_HOME``,
      ``$REGISTRY``, ``$SCENARIO_TMP``).
    - SP4: ``display_map()`` returns the pair
      ``(sanitize_map, repo_map)`` where
      ``sanitize_map = {actual_repo: display_name}``
      (input to the cast sanitisation step) and
      ``repo_map = {display_name: actual_repo}``
      (input to the command rewriter).
    - SP8 (Living Design Record, 2026-05-17): ``work_dir`` is the filesystem
      directory the provider's setup function actually wrote its inputs into
      (e.g. ``build/``, ``metadata.json`` for the publisher state).  The
      recordings runner ``cd``s to this directory before replaying commands so
      that relative paths in publisher scripts resolve correctly.  ``None``
      for providers that do not write a publisher-style work tree (all
      scenario-family adapters; all non-publisher setup-family adapters).
      This property is **recordings-only** — the drift-gate executor
      (``run_doc_script``) does not consult it.
    - DE2: ``declared_display_env()`` is a **static, zero-I/O** accessor that
      returns ``{PKG_<KEY>: <canonical_short_ref>}`` derived from the
      provider's declared package names.  No ``provision()`` / ``setup()``
      call may occur inside this method.  KEY derivation mirrors
      ``_build_script_env_from_packages`` (``display_name.upper().replace
      ("-", "_")``).  Values are canonical short refs (e.g. ``"uv:0.10"``),
      never UUID-prefixed actual repo names.
    """

    packages: dict[str, PackageInfo]
    """Packages keyed by display name (SP2)."""

    work_dir: Path | None
    """Working directory the setup function wrote its inputs into (SP8).

    ``None`` for providers that do not create a publisher-style work tree.
    Set only after ``provision()`` has been called.
    """

    def script_env(self) -> dict[str, str]:
        """Return the Scenario-style env projection for the script runner.

        Variables emitted (SP3):
        - Per-package: ``PKG_<KEY>``, ``FQ_<KEY>``, ``REPO_<KEY>``,
          ``TAG_<KEY>``, ``MARKER_<KEY>``, ``HOME_KEY_<KEY>``
          (KEY = uppercased, hyphens replaced with underscores).
        - Runner-level: ``OCX``, ``OCX_HOME``, ``REGISTRY``,
          ``SCENARIO_TMP``.
        """
        ...

    def declared_display_env(self) -> dict[str, str]:
        """Return the static, zero-I/O display-env projection (DE2).

        Returns ``{PKG_<KEY>: <canonical_short_ref>}`` derived from the
        provider's **declared** package names (a ``DECLARED_PACKAGES``
        class/instance attribute or equivalent static surface) without
        calling ``provision()`` / ``setup()`` or performing any I/O.

        KEY derivation: ``display_name.upper().replace("-", "_")``, matching
        ``_build_script_env_from_packages`` exactly so the rendered token
        namespace is identical to the runtime ``$PKG_*`` namespace.

        Values are canonical short refs (e.g. ``"uv:0.10"``), never the
        SP7 UUID-prefixed actual repo names (which do not exist statically).

        Returns ``{}`` for providers with no declared packages.
        """
        ...

    def display_map(self) -> tuple[dict[str, str], dict[str, str]]:
        """Return ``(sanitize_map, repo_map)`` for cast output rewriting.

        ``sanitize_map`` maps ``{actual_repo: display_name}`` — used by the
        cast sanitisation pass to replace UUID-prefixed repo names with the
        human-readable display names shown in the cast.

        ``repo_map`` maps ``{display_name: actual_repo}`` — used by the
        command rewriter to translate display-name package references in
        script command text back to the actual UUID-prefixed repo names
        before executing them (SP4).

        The pair is returned together because both are derived from the same
        ``packages`` dict and are always consumed together.
        """
        ...

    def provision(self, ocx: OcxRunner, tmp_path: Path) -> None:
        """Perform all registry pushes required by this state.

        This is the **only** method that may perform OCI / network I/O (SP0).
        After this call returns, ``self.packages`` is populated and
        ``script_env()`` / ``display_map()`` return correct values.

        Must be idempotent: calling provision a second time with the same
        arguments on the same instance must not raise.
        """
        ...


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


def _build_script_env_from_packages(
    packages: dict[str, PackageInfo],
    ocx: OcxRunner,
    tmp_path: Path,
) -> dict[str, str]:
    """Synthesize a Scenario-style env projection from a packages dict.

    Mirrors Scenario.script_env() exactly (SP3):
      - Per-package: PKG_<KEY>, FQ_<KEY>, REPO_<KEY>, TAG_<KEY>,
        MARKER_<KEY>, HOME_KEY_<KEY>  (KEY = upper, hyphens → underscores)
      - Runner-level: OCX, OCX_HOME, REGISTRY, SCENARIO_TMP, PATH
    """
    env: dict[str, str] = ocx.env.copy()
    bin_dir = str(ocx.binary.parent)
    env["PATH"] = bin_dir + os.pathsep + env.get("PATH", "")
    env["OCX"] = str(ocx.binary)
    env["OCX_HOME"] = str(ocx.ocx_home)
    env["REGISTRY"] = ocx.registry
    env["SCENARIO_TMP"] = str(tmp_path)
    for display_name, pkg in packages.items():
        upper = display_name.upper().replace("-", "_")
        env[f"PKG_{upper}"] = pkg.short
        env[f"FQ_{upper}"] = pkg.fq
        env[f"REPO_{upper}"] = pkg.repo
        env[f"TAG_{upper}"] = pkg.tag
        env[f"MARKER_{upper}"] = pkg.marker
        env[f"HOME_KEY_{upper}"] = pkg.repo.upper().replace("-", "_") + "_HOME"
    return env


def _build_display_maps(
    packages: dict[str, PackageInfo],
) -> tuple[dict[str, str], dict[str, str]]:
    """Derive (sanitize_map, repo_map) from a display-name → PackageInfo dict.

    Mirrors test_recordings.py's construction (SP4):
      sanitize_map = {actual_repo: display_name}  — for entries where repo ≠ display_name
      repo_map     = {display_name: actual_repo}   — first occurrence wins

    Only entries where ``pkg.repo != display_name`` are included, matching
    the exact logic in test_recordings.test_record().
    """
    sanitize_map: dict[str, str] = {}
    repo_map: dict[str, str] = {}
    for display_name, pkg in packages.items():
        if pkg.repo != display_name:
            sanitize_map[pkg.repo] = display_name
            if display_name not in repo_map:
                repo_map[display_name] = pkg.repo
    return sanitize_map, repo_map


# ---------------------------------------------------------------------------
# Declared display-env surface (DE2 — Living Design Record shape)
# ---------------------------------------------------------------------------

# Module-level static table keyed by the **family-qualified state name**
# (``"setup:<name>"`` / ``"scenario:<Name>"``) → {display_key: short_ref}.
# Hand-authored literal; SP0-safe (no provision()/I/O). DE6 cross-checks each
# entry against the provisioned truth; DE0 is the oracle. NOT a shared class
# attribute on the adapters (those wrap different providers per name — a
# shared class dict cannot hold per-name values; DE2 LDR).
#
# Values are canonical short refs (no SP7 UUID prefix). This is the SOLE
# source for declared_display_env() (always static, DE2/DE4); DE3/DE6 are
# separate cross-checks that this table equals the provisioned pkg.short.
DECLARED_PACKAGES: dict[str, dict[str, str]] = {
    # ---- setup family (recordings.setups.SETUPS) ----
    "setup:basic": {
        "uv": "uv:0.10.0",
    },
    "setup:multi-version": {
        # versions[0].short — "first version wins" (DE6 guards ordering)
        "corretto": "corretto:21.0.0",
    },
    "setup:full-catalog": {
        "uv": "uv:0.10.0",
        "cmake": "cmake:4.2.0",
        "corretto": "corretto:21.0.0",
        "ocx": "ocx:0.1.0",
        "nodejs": "nodejs:24.0.0",
        "bun": "bun:1.3.0",
    },
    "setup:variants": {
        # versions[0].short — "first version wins" (DE6 guards ordering)
        "python": "python:pgo.lto-3.13.0",
    },
    "setup:dependencies": {
        "nodejs": "nodejs:24.0.0",
        "bun": "bun:1.3.0",
        "webapp": "webapp:2.0.0",
    },
    "setup:deps-visibility": {
        "nodejs": "nodejs:24.0.0",
        "bun": "bun:1.3.0",
        "templates": "templates:1.0.0",
        "server": "server:1.0.0",
        "renderer": "renderer:1.0.0",
        "webapp": "webapp:2.0.0",
    },
    "setup:publisher": {
        # publisher exposes display name "mytool" with stub tag "display"
        "mytool": "mytool:display",
    },
    # ---- scenario family (src.scenarios.SCENARIOS) ----
    "scenario:BasicPackage": {
        "hello": "hello:1.0.0",
    },
    "scenario:DiamondDeps": {
        "leaf": "leaf:1.0.0",
        "left": "left:1.0.0",
        "right": "right:1.0.0",
        "app": "app:1.0.0",
    },
    "scenario:MultiEntrypoints": {
        "toolkit": "toolkit:1.0.0",
    },
    "scenario:MultiLayer": {
        # display name "pkg", repo base "multilayer"
        "pkg": "multilayer:1.0.0",
    },
    "scenario:ThreeLevelDeps": {
        "leaf": "leaf:1.0.0",
        "mid": "mid:1.0.0",
        "app": "app:1.0.0",
    },
    "scenario:TwoLevelDeps": {
        "leaf": "leaf:1.0.0",
        "app": "app:1.0.0",
    },
}


def _project_declared_display_env(state_key: str) -> dict[str, str]:
    """Project ``DECLARED_PACKAGES[state_key]`` to the renderable-var matrix.

    Renderable matrix (LDR 2026-05-17, RN3 / DE1 / DE2) — two static,
    reader-facing, SP0-safe forms per declared package:

    - ``PKG_<KEY>``  → ``<short_ref>``      (e.g. ``corretto:21.0.0``)
    - ``REPO_<KEY>`` → ``<repo>`` = short_ref minus its ``:<tag>``
      (e.g. ``corretto``) — the bare repo name a reader types for
      ``ocx index list``, version-qualified refs ``"$REPO_X:25.0.0"``, etc.

    ``<KEY>`` derivation matches ``_build_script_env_from_packages`` exactly
    (``display_name.upper().replace("-", "_")``) so the rendered token
    namespace equals the runtime ``$PKG_*`` / ``$REPO_*`` namespace.  Both
    forms are static (no provision/I/O — DE4); ``$FQ_*`` / ``$TAG_*`` /
    ``$MARKER_*`` / ``$HOME_KEY_*`` and runner vars stay non-renderable
    (RN5).  Returns ``{}`` when the state declares no packages (DE3).

    Sole implementation of ``declared_display_env()`` (always static — DE2;
    SP0-safe — DE4). DE3/DE6 separately verify this equals provisioned truth.
    """
    pkg_map = DECLARED_PACKAGES.get(state_key, {})
    out: dict[str, str] = {}
    for display_name, short_ref in pkg_map.items():
        key = display_name.upper().replace("-", "_")
        out[f"PKG_{key}"] = short_ref
        out[f"REPO_{key}"] = short_ref.rsplit(":", 1)[0]
    return out


# ---------------------------------------------------------------------------
# SetupAdapter
# ---------------------------------------------------------------------------


class SetupAdapter:
    """Wraps a legacy ``recordings.setups.SETUPS`` function as a StateProvider.

    The wrapped function has the signature::

        fn(ocx: OcxRunner, tmp_path: Path, prefix: str = "")
            -> dict[str, list[PackageInfo]]

    ``SetupAdapter`` is **behaviour-equivalent** to calling the underlying
    function directly (SP5): after ``provision()`` the ``packages`` dict
    contains the first ``PackageInfo`` for each display name, matching what
    the legacy recordings suite reads directly.

    Construction performs zero I/O (SP0).

    Declared display-env (DE2 / ADR H-1): sourced from the **module-level**
    ``DECLARED_PACKAGES`` table keyed by the family-qualified state name
    ``f"setup:{self._name}"`` — *not* a class attribute (DE2 LDR).
    """

    def __init__(self, name: str, fn: object) -> None:
        """Initialise adapter without invoking the setup function (SP0).

        Args:
            name: The recordings-family name (e.g. ``"basic"``).  Used for
                repr / error messages; the fully-qualified state key is
                ``setup:<name>``.
            fn: The callable from ``recordings.setups.SETUPS``.  Signature:
                ``(ocx, tmp_path, prefix="") -> dict[str, list[PackageInfo]]``.
        """
        self._name = name
        self._fn = fn
        self.packages: dict[str, PackageInfo] = {}
        """Packages keyed by display name (SP2). Empty until provision() is called."""
        # Stored after provision() so script_env() / display_map() can use them.
        self._ocx: OcxRunner | None = None
        self._tmp_path: Path | None = None
        self.work_dir: Path | None = None
        """Actual directory passed to the SETUPS function (SP8).  Set after provision()."""

    def script_env(self) -> dict[str, str]:
        """Return the Scenario-style env projection (SP3).

        Synthesizes the $PKG_* projection from the provisioned packages.
        Requires ``provision()`` to have been called first.
        """
        if self._ocx is None or self._tmp_path is None:
            raise RuntimeError(
                f"SetupAdapter({self._name!r}).script_env() called before provision()"
            )
        return _build_script_env_from_packages(self.packages, self._ocx, self._tmp_path)

    def display_map(self) -> tuple[dict[str, str], dict[str, str]]:
        """Return ``(sanitize_map, repo_map)`` (SP4).

        Derived from ``self.packages``.  Requires ``provision()`` to have
        been called first.
        """
        return _build_display_maps(self.packages)

    def declared_display_env(self) -> dict[str, str]:
        """Return the **static** zero-I/O display-env projection (DE2/DE4).

        Purely ``_project_declared_display_env(f"setup:{self._name}")`` —
        the module-level ``DECLARED_PACKAGES`` table only.  It **never**
        reads ``self.packages``: the accessor must be identical before and
        after ``provision()`` (DE2) and SP0-safe (DE4).  DE3/DE6 are
        *separate* cross-checks that compare this static value against the
        provisioned truth; reading ``self.packages`` here would make them
        trivially pass and defeat their purpose (catching a stale table).
        ``{}`` when the state declares no packages.
        """
        return _project_declared_display_env(f"setup:{self._name}")

    def provision(self, ocx: OcxRunner, tmp_path: Path) -> None:
        """Call the wrapped setup function and populate ``self.packages`` (SP0, SP5, SP7).

        Generates a **unique repo prefix** per provision call (SP7) so that
        concurrent xdist workers pushing the same setup do not collide on
        fixed repo names in the shared registry:2 instance.  The prefix
        follows the ``unique_repo`` convention from ``subsystem-tests.md``:
        ``t_<8hex>_`` (e.g. ``t_a1b2c3d4_``).

        The prefix changes only ``PackageInfo.repo`` / ``.fq``; the
        ``packages`` dict is still keyed by display names (SP2/SP5), and
        ``display_map()`` still maps the prefixed actual repo ↔ display name
        so the inverse property (SP4) is preserved.

        Registry I/O occurs here and only here.

        Uses a ``_state/`` subdirectory under ``tmp_path`` as the working
        directory for the setup function so that a test can also call the
        legacy function directly with ``tmp_path`` without directory collisions
        (``make_package`` creates ``tmp_path/pkg-<repo>-<tag>/`` subdirs).
        ``SCENARIO_TMP`` is set to the subdirectory so that scripts that use
        relative paths (e.g. the publisher setup) work correctly.
        """
        fn = self._fn
        if not callable(fn):
            raise TypeError(f"SETUPS[{self._name!r}] is not callable: {fn!r}")
        prefix = f"t_{uuid4().hex[:8]}_"
        state_path = tmp_path / "_state"
        state_path.mkdir(parents=True, exist_ok=True)
        raw: dict[str, list[PackageInfo]] = fn(ocx, state_path, prefix)  # type: ignore[call-arg]
        # Pick the first (primary) version for each display name — matches
        # the legacy recordings suite which iterates all versions but uses
        # index 0 as the canonical entry for sanitise_map / repo_map.
        self.packages = {
            display_name: versions[0]
            for display_name, versions in raw.items()
            if versions
        }
        self._ocx = ocx
        self._tmp_path = state_path
        # SP8: expose the directory the setup function wrote into so the
        # recordings runner can cd to the correct working directory.
        self.work_dir = state_path

    def __repr__(self) -> str:
        return f"SetupAdapter(setup:{self._name!r})"


# ---------------------------------------------------------------------------
# ScenarioAdapter
# ---------------------------------------------------------------------------


class ScenarioAdapter:
    """Wraps a legacy ``src.scenarios.Scenario`` subclass as a StateProvider.

    The wrapped class has a ``name`` class-level attribute (the registry key,
    e.g. ``"BasicPackage"``), a ``setup()`` method that publishes packages
    into ``self.packages``, and a ``script_env()`` method returning the
    standard env projection.

    ``ScenarioAdapter`` is **behaviour-equivalent** to instantiating the
    subclass and calling ``setup()`` directly (SP6): the same ``$PKG_*``
    projection is exposed.

    Construction performs zero I/O (SP0).

    Declared display-env (DE2 / ADR H-1): sourced from the **module-level**
    ``DECLARED_PACKAGES`` table keyed by the family-qualified state name
    ``f"scenario:{self._name}"`` — *not* a class attribute (DE2 LDR).
    """

    def __init__(self, name: str, cls: type) -> None:
        """Initialise adapter without instantiating the scenario class (SP0).

        Args:
            name: The scenario-family name (e.g. ``"BasicPackage"``).  Used
                for repr / error messages; the fully-qualified state key is
                ``scenario:<name>``.
            cls: The ``Scenario`` subclass from ``src.scenarios.SCENARIOS``.
        """
        self._name = name
        self._cls = cls
        self._instance: object | None = None  # Scenario instance, set by provision()
        self.packages: dict[str, PackageInfo] = {}
        """Packages keyed by display name (SP2). Empty until provision() is called."""
        self.work_dir: Path | None = None
        """Always None for scenario-family adapters (SP8)."""

    # -- StateProvider members --

    def script_env(self) -> dict[str, str]:
        """Return the Scenario-style env projection (SP3).

        Delegates directly to the instantiated scenario's ``script_env()``
        method.  Requires ``provision()`` to have been called first.
        """
        if self._instance is None:
            raise RuntimeError(
                f"ScenarioAdapter({self._name!r}).script_env() called before provision()"
            )
        return self._instance.script_env()  # type: ignore[union-attr]

    def display_map(self) -> tuple[dict[str, str], dict[str, str]]:
        """Return ``(sanitize_map, repo_map)`` (SP4).

        Derived from ``self.packages``.  Requires ``provision()`` to have
        been called first.
        """
        return _build_display_maps(self.packages)

    def declared_display_env(self) -> dict[str, str]:
        """Return the **static** zero-I/O display-env projection (DE2/DE4).

        Purely ``_project_declared_display_env(f"scenario:{self._name}")`` —
        the module-level ``DECLARED_PACKAGES`` table only.  It **never**
        reads ``self.packages`` (identical before/after ``provision()`` —
        DE2; SP0-safe — DE4).  DE3/DE6 are *separate* cross-checks of this
        static value against provisioned truth.  ``{}`` when the state
        declares no packages.
        """
        return _project_declared_display_env(f"scenario:{self._name}")

    def provision(self, ocx: OcxRunner, tmp_path: Path) -> None:
        """Instantiate + call ``setup()`` on the wrapped scenario class (SP0, SP6).

        Registry I/O occurs here and only here.
        """
        from src.scenarios import Scenario
        instance: Scenario = self._cls(ocx, tmp_path)
        instance.setup()
        self._instance = instance
        self.packages = instance.packages

    def __repr__(self) -> str:
        return f"ScenarioAdapter(scenario:{self._name!r})"


# ---------------------------------------------------------------------------
# Registry + resolver
# ---------------------------------------------------------------------------

def _build_registry() -> dict[str, StateProvider]:
    """Build the unified provider registry without invoking any setup (SP0).

    Imports legacy registries lazily inside this function so that import
    side-effects (subclass registration for SCENARIOS) stay contained.

    Returns:
        A dict mapping fully-qualified state keys to concrete adapters:
        ``{"setup:basic": SetupAdapter(...), "scenario:BasicPackage": ScenarioAdapter(...), ...}``
    """
    from recordings.setups import SETUPS
    from src.scenarios import SCENARIOS

    registry: dict[str, StateProvider] = {}

    for name, fn in SETUPS.items():
        registry[f"setup:{name}"] = SetupAdapter(name, fn)

    for name, cls in SCENARIOS.items():
        registry[f"scenario:{name}"] = ScenarioAdapter(name, cls)

    return registry


# Implementation fills this via _build_registry(); stub initialises to empty
# so the module imports cleanly.  The resolved dict maps fully-qualified state
# keys (``"setup:basic"``, ``"scenario:BasicPackage"``, …) to adapters.
STATE_PROVIDERS: dict[str, StateProvider] = _build_registry()

# Sorted, de-duplicated family prefixes (``setup``, ``scenario``, …) derived
# once from the registry.  Computed at module scope (Perf F2) so the EX4
# error path in ``resolve_state`` does not re-derive + double-sort this on
# every miss.  The registry is built once at import (no mutation after), so a
# module constant is sound.
_AVAILABLE_FAMILIES: tuple[str, ...] = tuple(
    sorted({k.split(":", 1)[0] for k in STATE_PROVIDERS})
)


def resolve_state(state: str) -> StateProvider:
    """Resolve a fully-qualified state key to its ``StateProvider``.

    Args:
        state: A family-qualified state string: ``"setup:<name>"`` or
            ``"scenario:<Name>"``.  An unqualified string (no ``setup:`` /
            ``scenario:`` prefix) is **always rejected** — the caller (not
            this function) is responsible for applying the default
            ``"setup:basic"`` when the ``# state:`` header is absent (EX6).

    Returns:
        The registered ``StateProvider`` for ``state``.

    Raises:
        ValueError: If ``state`` is unqualified, unknown, or the named key
            does not exist in any family.  Message form (EX4)::

                invalid state '<state>'; expected setup:<name> or
                scenario:<Name>; available: <sorted families>
    """
    # Reject unqualified states (no family prefix) — EX4
    is_qualified = state.startswith("setup:") or state.startswith("scenario:")
    if not is_qualified or state not in STATE_PROVIDERS:
        available = ", ".join(f"{f}:..." for f in _AVAILABLE_FAMILIES)
        raise ValueError(
            f"invalid state {state!r}; expected setup:<name> or scenario:<Name>; "
            f"available: {available}"
        )
    return STATE_PROVIDERS[state]


__all__ = [
    "STATE_PROVIDERS",
    "ScenarioAdapter",
    "SetupAdapter",
    "StateProvider",
    "resolve_state",
]
