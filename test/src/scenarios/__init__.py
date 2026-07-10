"""Scenario harness for shell-driven acceptance tests.

A `Scenario` orchestrates registry state (publish packages, install them)
and runs bash scripts against that state. Scripts can be passed as a path,
a string, or auto-discovered from a `# scenario: <Name>` header.

Three usage shapes:

1. **Inline** — `Scenario(ocx, tmp_path).run_inline("ocx install foo:1")`
2. **File** — `Scenario(ocx, tmp_path).run_file(Path("script.sh"))`
3. **Auto-load** — `Scenario.auto_load(script_path, ocx, tmp_path)` parses
   `# scenario: TwoLevelDeps`, instantiates the registered subclass, calls
   its `setup()`, then returns a ready-to-run scenario.

Subclasses register themselves via class-level `name = "..."` attribute and
`__init_subclass__` hook. Each subclass overrides `setup()` to call
`self.publish(...)` / `self.install(...)`; published packages land in
`self.packages` and are exposed to scripts as bash env vars (`$PKG_<KEY>`,
`$FQ_<KEY>`, `$REPO_<KEY>`, `$TAG_<KEY>`, `$MARKER_<KEY>`).
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path
from typing import Any, ClassVar
from uuid import uuid4

from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo

SCENARIOS: dict[str, type["Scenario"]] = {}


class Scenario:
    """Base scenario. Subclass to pre-publish state in `setup()`."""

    name: ClassVar[str] = ""

    def __init_subclass__(cls, **kwargs: Any) -> None:
        super().__init_subclass__(**kwargs)
        if cls.name:
            SCENARIOS[cls.name] = cls

    def __init__(self, ocx: OcxRunner, tmp_path: Path) -> None:
        self.ocx = ocx
        self.tmp_path = tmp_path
        self.packages: dict[str, PackageInfo] = {}
        # UUID prefix keeps repo names unique across parallel test workers
        # sharing the session-scoped registry.
        self.prefix = f"s_{uuid4().hex[:8]}_"

    # -- subclass hook --

    def setup(self) -> None:
        """Override to publish packages and populate `self.packages`."""

    # -- state construction --

    def repo(self, name: str) -> str:
        """Return the prefixed, registry-unique repo name for `name`."""
        return f"{self.prefix}{name}"

    def publish(self, name: str, tag: str = "1.0.0", **kw: Any) -> PackageInfo:
        """Publish a package, store it as `self.packages[name]`, and return it."""
        pkg = make_package(self.ocx, self.repo(name), tag, self.tmp_path, **kw)
        self.packages[name] = pkg
        return pkg

    def publish_with_deps(
        self,
        name: str,
        tag: str = "1.0.0",
        *,
        deps: list[tuple[str, str | None]] | None = None,
        **kw: Any,
    ) -> PackageInfo:
        """Publish a package that depends on previously-published packages.

        `deps` is a list of `(scenario_key, visibility)` tuples — the key
        must already be in `self.packages`; visibility is `"public"`,
        `"private"`, `"interface"`, `"sealed"`, or `None` (omit field).
        """
        dep_entries: list[dict[str, Any]] = []
        for key, visibility in deps or []:
            dep_pkg = self.packages[key]
            # Dependency pins must be platform MANIFEST digests — the push
            # gate rejects index digests (adr_dependency_manifest_pinning.md).
            digest = fetch_platform_manifest_digest(self.ocx.registry, dep_pkg.repo, dep_pkg.tag)
            entry: dict[str, Any] = {"identifier": f"{dep_pkg.fq}@{digest}"}
            if visibility is not None:
                entry["visibility"] = visibility
            dep_entries.append(entry)
        pkg = make_package(
            self.ocx, self.repo(name), tag, self.tmp_path,
            dependencies=dep_entries, **kw,
        )
        self.packages[name] = pkg
        return pkg

    def install(self, *names: str) -> None:
        """Install one or more previously-published packages by scenario key."""
        for name in names:
            self.ocx.plain("install", "--select", self.packages[name].short)

    # -- script execution --

    def script_env(self, extra: dict[str, str] | None = None) -> dict[str, str]:
        """Bash environment exposed to scripts.

        Includes the OcxRunner env (OCX_HOME, OCX_DEFAULT_REGISTRY,
        OCX_INSECURE_REGISTRIES, PATH), the path to the ocx binary
        prepended to PATH (so scripts can call `ocx ...` directly), and
        per-package variables.
        """
        env = self.ocx.env.copy()
        bin_dir = str(self.ocx.binary.parent)
        env["PATH"] = bin_dir + os.pathsep + env.get("PATH", "")
        env["OCX"] = str(self.ocx.binary)
        env["OCX_HOME"] = str(self.ocx.ocx_home)
        env["REGISTRY"] = self.ocx.registry
        env["SCENARIO_TMP"] = str(self.tmp_path)
        for key, pkg in self.packages.items():
            upper = key.upper().replace("-", "_")
            env[f"PKG_{upper}"] = pkg.short
            env[f"FQ_{upper}"] = pkg.fq
            env[f"REPO_{upper}"] = pkg.repo
            env[f"TAG_{upper}"] = pkg.tag
            env[f"MARKER_{upper}"] = pkg.marker
            # Env-var name make_package injects per package (e.g. "FOO_BAR_HOME"
            # for repo "foo-bar"). Scripts use this to grep for transitive deps.
            env[f"HOME_KEY_{upper}"] = pkg.repo.upper().replace("-", "_") + "_HOME"
        if extra:
            env.update(extra)
        return env

    def run_inline(
        self,
        body: str,
        *,
        env: dict[str, str] | None = None,
        check: bool = True,
        cwd: Path | None = None,
    ) -> subprocess.CompletedProcess[str]:
        """Run an inline bash script body with the scenario environment."""
        full_env = self.script_env(env)
        result = subprocess.run(
            ["bash", "-c", body],
            env=full_env,
            cwd=str(cwd) if cwd else str(self.tmp_path),
            capture_output=True,
            text=True,
        )
        if check and result.returncode != 0:
            raise AssertionError(
                "scenario script failed "
                f"(rc={result.returncode})\n"
                f"--- stdout ---\n{result.stdout}\n"
                f"--- stderr ---\n{result.stderr}"
            )
        return result

    def run_file(
        self, path: Path, **kw: Any
    ) -> subprocess.CompletedProcess[str]:
        """Run a bash script from a file with the scenario environment."""
        return self.run_inline(path.read_text(), **kw)

    # -- auto-load --

    @classmethod
    def auto_load(
        cls,
        script_path: Path,
        ocx: OcxRunner,
        tmp_path: Path,
    ) -> "Scenario":
        """Instantiate the scenario named in the script's `# scenario:` header.

        If no header is present, returns a base `Scenario` (no setup).
        Always calls `setup()` before returning.
        """
        meta = parse_script_metadata(script_path)
        scenario_name = meta.get("scenario")
        if scenario_name:
            sub = SCENARIOS.get(scenario_name)
            if sub is None:
                available = ", ".join(sorted(SCENARIOS)) or "(none registered)"
                raise ValueError(
                    f"unknown scenario {scenario_name!r} in {script_path.name}; "
                    f"available: {available}"
                )
            instance: Scenario = sub(ocx, tmp_path)
        else:
            instance = cls(ocx, tmp_path)
        instance.setup()
        return instance


# ---------------------------------------------------------------------------
# Script header parsing + discovery
# ---------------------------------------------------------------------------


def parse_script_metadata(path: Path) -> dict[str, str]:
    """Parse `# key: value` headers from the top of a shell script.

    Stops at the first non-comment, non-blank line. Recognised keys are
    lowercased; values are stripped. Shebang line is ignored.
    """
    meta: dict[str, str] = {}
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if not line:
            continue
        if line.startswith("#!"):
            continue
        if not line.startswith("#"):
            break
        rest = line[1:].strip()
        if ":" not in rest:
            continue
        key, _, value = rest.partition(":")
        meta[key.strip().lower()] = value.strip()
    return meta


def discover_scripts(root: Path, pattern: str = "**/*.sh") -> list[Path]:
    """Return all `.sh` files under `root` matching `pattern`, sorted."""
    if not root.exists():
        return []
    return sorted(root.glob(pattern))


# ---------------------------------------------------------------------------
# Eager-import predefined scenarios so registration runs at module load.
# Keep this at the bottom to avoid circular imports.
# ---------------------------------------------------------------------------
from src.scenarios import basic  # noqa: E402,F401
from src.scenarios import diamond_deps  # noqa: E402,F401
from src.scenarios import multi_entrypoints  # noqa: E402,F401
from src.scenarios import multi_layer  # noqa: E402,F401
from src.scenarios import three_level_deps  # noqa: E402,F401
from src.scenarios import two_level_deps  # noqa: E402,F401

__all__ = [
    "SCENARIOS",
    "Scenario",
    "discover_scripts",
    "parse_script_metadata",
]
