"""Recording environment setups.

Each setup is a function that publishes packages into the test registry
and returns a dict mapping display names to lists of PackageInfo.
Shell scripts reference a setup via ``# setup: <name>``.
"""
from __future__ import annotations

from collections.abc import Callable
from pathlib import Path

from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo


def basic(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """Single package, one version."""
    uv_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
    ]
    return {
        "uv": [
            make_package(
                ocx, "uv", "0.10.0", tmp_path, bins=["uv"], env=uv_env,
                outputs={"uv": {"--version": "uv 0.10.10"}},
            ),
        ],
    }


def multi_version(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """One package with multiple versions."""
    corretto_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "JAVA_HOME", "type": "constant", "value": "${installPath}"},
    ]
    return {
        "corretto": [
            make_package(
                ocx, "corretto", "21.0.0", tmp_path,
                bins=["java", "javac"], env=corretto_env,
                outputs={"java": {"-version": (
                    "openjdk 21.0.10 2026-01-20 LTS\n"
                    "OpenJDK Runtime Environment Corretto-21.0.10.7.1 (build 21.0.10+7-LTS)\n"
                    "OpenJDK 64-Bit Server VM Corretto-21.0.10.7.1 (build 21.0.10+7-LTS, mixed mode, sharing)"
                )}},
            ),
            make_package(
                ocx, "corretto", "25.0.0", tmp_path,
                bins=["java", "javac"], env=corretto_env, new=False,
                outputs={"java": {"-version": (
                    "openjdk 25.0.2 2026-01-20 LTS\n"
                    "OpenJDK Runtime Environment Corretto-25.0.2.10.1 (build 25.0.2+10-LTS)\n"
                    "OpenJDK 64-Bit Server VM Corretto-25.0.2.10.1 (build 25.0.2+10-LTS, mixed mode, sharing)"
                )}},
            ),
        ],
    }


def full_catalog(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """Multiple packages for index/catalog demos."""
    uv_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
    ]
    cmake_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "MANPATH", "type": "path", "value": "${installPath}/share/man"},
    ]
    corretto_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "JAVA_HOME", "type": "constant", "value": "${installPath}"},
    ]
    node_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
    ]
    bun_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
    ]
    ocx_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
    ]
    return {
        "uv": [
            make_package(
                ocx, "uv", "0.10.0", tmp_path, bins=["uv"], env=uv_env,
                outputs={"uv": {"--version": "uv 0.10.10"}},
            ),
        ],
        "cmake": [
            make_package(
                ocx, "cmake", "4.2.0", tmp_path, bins=["cmake"], env=cmake_env,
                outputs={"cmake": {"--version": (
                    "cmake version 4.2.3\n"
                    "\n"
                    "CMake suite maintained and supported by Kitware (kitware.com/cmake)."
                )}},
            ),
        ],
        "corretto": [
            make_package(
                ocx, "corretto", "21.0.0", tmp_path,
                bins=["java", "javac"], env=corretto_env,
                outputs={"java": {"-version": (
                    "openjdk 21.0.10 2026-01-20 LTS\n"
                    "OpenJDK Runtime Environment Corretto-21.0.10.7.1 (build 21.0.10+7-LTS)\n"
                    "OpenJDK 64-Bit Server VM Corretto-21.0.10.7.1 (build 21.0.10+7-LTS, mixed mode, sharing)"
                )}},
            ),
            make_package(
                ocx, "corretto", "25.0.0", tmp_path,
                bins=["java", "javac"], env=corretto_env, new=False,
                outputs={"java": {"-version": (
                    "openjdk 25.0.2 2026-01-20 LTS\n"
                    "OpenJDK Runtime Environment Corretto-25.0.2.10.1 (build 25.0.2+10-LTS)\n"
                    "OpenJDK 64-Bit Server VM Corretto-25.0.2.10.1 (build 25.0.2+10-LTS, mixed mode, sharing)"
                )}},
            ),
        ],
        "ocx": [
            make_package(
                ocx, "ocx", "0.1.0", tmp_path, bins=["ocx"], env=ocx_env,
                outputs={"ocx": {"--version": "ocx 0.1.0"}},
            ),
        ],
        "nodejs": [
            make_package(
                ocx, "nodejs", "24.0.0", tmp_path, bins=["node"], env=node_env,
                outputs={"node": {"--version": "v24.14.0"}},
            ),
        ],
        "bun": [
            make_package(
                ocx, "bun", "1.3.0", tmp_path, bins=["bun"], env=bun_env,
                outputs={"bun": {"--version": "1.3.10"}},
            ),
        ],
    }


def variants(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """Python with multiple variant builds for variant discovery demos."""
    python_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "PYTHON_HOME", "type": "constant", "value": "${installPath}"},
    ]
    return {
        "python": [
            make_package(
                ocx, "python", "pgo.lto-3.13.0", tmp_path / "pgo-lto",
                bins=["python3"], env=python_env,
                outputs={"python3": {"--version": "Python 3.13.4"}},
            ),
            make_package(
                ocx, "python", "debug-3.13.0", tmp_path / "debug",
                bins=["python3"], env=python_env, new=False,
                outputs={"python3": {"--version": "Python 3.13.4 (debug)"}},
            ),
            make_package(
                ocx, "python", "freethreaded-3.13.0", tmp_path / "freethreaded",
                bins=["python3"], env=python_env, new=False,
                outputs={"python3": {"--version": "Python 3.13.4 (freethreaded)"}},
            ),
        ],
    }


def dependencies(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """Packages with dependency relationships for deps command demos.

    Dependency graph: webapp -> {nodejs, bun}
    """
    from src.registry import fetch_manifest_digest

    node_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "NODE_HOME", "type": "constant", "value": "${installPath}"},
    ]
    bun_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "BUN_HOME", "type": "constant", "value": "${installPath}"},
    ]
    webapp_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "APP_HOME", "type": "constant", "value": "${installPath}"},
    ]

    nodejs = make_package(
        ocx, "nodejs", "24.0.0", tmp_path, bins=["node"], env=node_env,
        outputs={"node": {"--version": "v24.14.0"}},
    )
    bun = make_package(
        ocx, "bun", "1.3.0", tmp_path, bins=["bun"], env=bun_env,
        outputs={"bun": {"--version": "1.3.10"}},
    )

    node_digest = fetch_manifest_digest(ocx.registry, nodejs.repo, nodejs.tag)
    bun_digest = fetch_manifest_digest(ocx.registry, bun.repo, bun.tag)
    node_dep = {"identifier": f"{nodejs.fq}@{node_digest}"}
    bun_dep = {"identifier": f"{bun.fq}@{bun_digest}"}

    webapp = make_package(
        ocx, "webapp", "2.0.0", tmp_path, bins=["serve"],
        env=webapp_env, dependencies=[node_dep, bun_dep],
        outputs={"serve": {"--version": "webapp 2.0.0"}},
    )

    return {
        "nodejs": [nodejs],
        "bun": [bun],
        "webapp": [webapp],
    }


def deps_visibility(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """Packages with mixed visibility levels for deps command demos.

    Diamond dependency graph through nodejs, with visibility annotations::

        webapp:2.0
        ├── server:1.0    (public)    → nodejs:24  (public)
        ├── bun:1.3       (public)
        ├── renderer:1.0  (private)   → nodejs:24  (*) (private)
        └── templates:1.0 (sealed)

    server is an Express-style framework that exports Node.js at runtime.
    bun is a standalone bundler (no transitive deps).
    renderer is an SSR engine that runs JS templates via Node.js — the
    webapp needs it internally for its shims but doesn't expose it to
    consumers (private). Creates the diamond through nodejs.
    templates is static content accessed by path — no env needed (sealed).

    Tree annotations: (private), (*), (sealed).
    Flat view: visibility column with all levels.
    Why view: two paths from webapp to nodejs.
    """
    from src.registry import fetch_manifest_digest

    path_env = [{"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"}]
    node_env = path_env + [{"key": "NODE_HOME", "type": "constant", "value": "${installPath}"}]
    bun_env = path_env + [{"key": "BUN_HOME", "type": "constant", "value": "${installPath}"}]

    # Leaf packages (no deps of their own)
    nodejs = make_package(
        ocx, "nodejs", "24.0.0", tmp_path, bins=["node"], env=node_env,
        outputs={"node": {"--version": "v24.14.0"}},
    )
    bun = make_package(
        ocx, "bun", "1.3.0", tmp_path, bins=["bun"], env=bun_env,
        outputs={"bun": {"--version": "1.3.10"}},
    )
    templates = make_package(
        ocx, "templates", "1.0.0", tmp_path, bins=["tpl"],
        outputs={"tpl": {"--version": "templates 1.0.0"}},
    )

    node_digest = fetch_manifest_digest(ocx.registry, nodejs.repo, nodejs.tag)
    bun_digest = fetch_manifest_digest(ocx.registry, bun.repo, bun.tag)
    templates_digest = fetch_manifest_digest(ocx.registry, templates.repo, templates.tag)

    # Intermediate packages (depend on nodejs)
    server = make_package(
        ocx, "server", "1.0.0", tmp_path, bins=["server"], env=path_env,
        dependencies=[{"identifier": f"{nodejs.fq}@{node_digest}", "visibility": "public"}],
        outputs={"server": {"--version": "server 1.0.0"}},
    )
    renderer = make_package(
        ocx, "renderer", "1.0.0", tmp_path, bins=["render"], env=path_env,
        dependencies=[{"identifier": f"{nodejs.fq}@{node_digest}", "visibility": "public"}],
        outputs={"render": {"--version": "renderer 1.0.0"}},
    )

    server_digest = fetch_manifest_digest(ocx.registry, server.repo, server.tag)
    renderer_digest = fetch_manifest_digest(ocx.registry, renderer.repo, renderer.tag)

    # Root: webapp depends on server (public), bun (public), renderer (private)
    webapp = make_package(
        ocx, "webapp", "2.0.0", tmp_path, bins=["serve"],
        env=path_env + [{"key": "APP_HOME", "type": "constant", "value": "${installPath}"}],
        dependencies=[
            {"identifier": f"{server.fq}@{server_digest}", "visibility": "public"},
            {"identifier": f"{bun.fq}@{bun_digest}", "visibility": "public"},
            {"identifier": f"{renderer.fq}@{renderer_digest}", "visibility": "private"},
            {"identifier": f"{templates.fq}@{templates_digest}"},
        ],
        outputs={"serve": {"--version": "webapp 2.0.0"}},
    )

    # Pre-install so recordings don't need `ocx install` on screen.
    ocx.run("install", "--select", webapp.short)

    return {
        "nodejs": [nodejs],
        "bun": [bun],
        "templates": [templates],
        "server": [server],
        "renderer": [renderer],
        "webapp": [webapp],
    }


SETUPS: dict[str, Callable] = {
    "basic": basic,
    "multi-version": multi_version,
    "full-catalog": full_catalog,
    "variants": variants,
    "dependencies": dependencies,
    "deps-visibility": deps_visibility,
}
