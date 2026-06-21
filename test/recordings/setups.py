"""Recording environment setups.

Each setup is a function that publishes packages into the test registry
and returns a dict mapping display names to lists of PackageInfo.
Shell scripts reference a setup via ``# setup: <name>``.
"""
from __future__ import annotations

import json
import stat
from collections.abc import Callable
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner, PackageInfo, current_platform


def basic(ocx: OcxRunner, tmp_path: Path, prefix: str = "") -> dict[str, list[PackageInfo]]:
    """Single package, one version."""
    uv_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
    ]
    return {
        "uv": [
            make_package(
                ocx, f"{prefix}uv", "0.10.0", tmp_path, bins=["uv"], env=uv_env,
                outputs={"uv": {"--version": "uv 0.10.10"}},
            ),
        ],
    }


def multi_version(ocx: OcxRunner, tmp_path: Path, prefix: str = "") -> dict[str, list[PackageInfo]]:
    """One package with multiple versions."""
    corretto_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
        {"key": "JAVA_HOME", "type": "constant", "value": "${installPath}",
         "visibility": "public"},
    ]
    return {
        "corretto": [
            make_package(
                ocx, f"{prefix}corretto", "21.0.0", tmp_path,
                bins=["java", "javac"], env=corretto_env,
                outputs={"java": {"-version": (
                    "openjdk 21.0.10 2026-01-20 LTS\n"
                    "OpenJDK Runtime Environment Corretto-21.0.10.7.1 (build 21.0.10+7-LTS)\n"
                    "OpenJDK 64-Bit Server VM Corretto-21.0.10.7.1 (build 21.0.10+7-LTS, mixed mode, sharing)"
                )}},
            ),
            make_package(
                ocx, f"{prefix}corretto", "25.0.0", tmp_path,
                bins=["java", "javac"], env=corretto_env, new=False,
                outputs={"java": {"-version": (
                    "openjdk 25.0.2 2026-01-20 LTS\n"
                    "OpenJDK Runtime Environment Corretto-25.0.2.10.1 (build 25.0.2+10-LTS)\n"
                    "OpenJDK 64-Bit Server VM Corretto-25.0.2.10.1 (build 25.0.2+10-LTS, mixed mode, sharing)"
                )}},
            ),
        ],
    }


def full_catalog(ocx: OcxRunner, tmp_path: Path, prefix: str = "") -> dict[str, list[PackageInfo]]:
    """Multiple packages for index/catalog demos."""
    uv_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
    ]
    cmake_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
        {"key": "MANPATH", "type": "path", "value": "${installPath}/share/man",
         "visibility": "public"},
    ]
    corretto_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
        {"key": "JAVA_HOME", "type": "constant", "value": "${installPath}",
         "visibility": "public"},
    ]
    node_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
    ]
    bun_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
    ]
    ocx_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
    ]
    return {
        "uv": [
            make_package(
                ocx, f"{prefix}uv", "0.10.0", tmp_path, bins=["uv"], env=uv_env,
                outputs={"uv": {"--version": "uv 0.10.10"}},
            ),
        ],
        "cmake": [
            make_package(
                ocx, f"{prefix}cmake", "4.2.0", tmp_path, bins=["cmake"], env=cmake_env,
                outputs={"cmake": {"--version": (
                    "cmake version 4.2.3\n"
                    "\n"
                    "CMake suite maintained and supported by Kitware (kitware.com/cmake)."
                )}},
            ),
        ],
        "corretto": [
            make_package(
                ocx, f"{prefix}corretto", "21.0.0", tmp_path,
                bins=["java", "javac"], env=corretto_env,
                outputs={"java": {"-version": (
                    "openjdk 21.0.10 2026-01-20 LTS\n"
                    "OpenJDK Runtime Environment Corretto-21.0.10.7.1 (build 21.0.10+7-LTS)\n"
                    "OpenJDK 64-Bit Server VM Corretto-21.0.10.7.1 (build 21.0.10+7-LTS, mixed mode, sharing)"
                )}},
            ),
            make_package(
                ocx, f"{prefix}corretto", "25.0.0", tmp_path,
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
                ocx, f"{prefix}ocx", "0.1.0", tmp_path, bins=["ocx"], env=ocx_env,
                outputs={"ocx": {"--version": "ocx 0.1.0"}},
            ),
        ],
        "nodejs": [
            make_package(
                ocx, f"{prefix}nodejs", "24.0.0", tmp_path, bins=["node"], env=node_env,
                outputs={"node": {"--version": "v24.14.0"}},
            ),
        ],
        "bun": [
            make_package(
                ocx, f"{prefix}bun", "1.3.0", tmp_path, bins=["bun"], env=bun_env,
                outputs={"bun": {"--version": "1.3.10"}},
            ),
        ],
    }


def variants(ocx: OcxRunner, tmp_path: Path, prefix: str = "") -> dict[str, list[PackageInfo]]:
    """Python with multiple variant builds for variant discovery demos."""
    python_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
        {"key": "PYTHON_HOME", "type": "constant", "value": "${installPath}",
         "visibility": "public"},
    ]
    return {
        "python": [
            make_package(
                ocx, f"{prefix}python", "pgo.lto-3.13.0", tmp_path / "pgo-lto",
                bins=["python3"], env=python_env,
                outputs={"python3": {"--version": "Python 3.13.4"}},
            ),
            make_package(
                ocx, f"{prefix}python", "debug-3.13.0", tmp_path / "debug",
                bins=["python3"], env=python_env, new=False,
                outputs={"python3": {"--version": "Python 3.13.4 (debug)"}},
            ),
            make_package(
                ocx, f"{prefix}python", "freethreaded-3.13.0", tmp_path / "freethreaded",
                bins=["python3"], env=python_env, new=False,
                outputs={"python3": {"--version": "Python 3.13.4 (freethreaded)"}},
            ),
        ],
    }


def dependencies(ocx: OcxRunner, tmp_path: Path, prefix: str = "") -> dict[str, list[PackageInfo]]:
    """Packages with dependency relationships for deps command demos.

    Dependency graph: webapp -> {nodejs, bun}
    """
    from src.registry import fetch_manifest_digest

    node_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
        {"key": "NODE_HOME", "type": "constant", "value": "${installPath}",
         "visibility": "public"},
    ]
    bun_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
        {"key": "BUN_HOME", "type": "constant", "value": "${installPath}",
         "visibility": "public"},
    ]
    webapp_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
        {"key": "APP_HOME", "type": "constant", "value": "${installPath}",
         "visibility": "public"},
    ]

    nodejs = make_package(
        ocx, f"{prefix}nodejs", "24.0.0", tmp_path, bins=["node"], env=node_env,
        outputs={"node": {"--version": "v24.14.0"}},
    )
    bun = make_package(
        ocx, f"{prefix}bun", "1.3.0", tmp_path, bins=["bun"], env=bun_env,
        outputs={"bun": {"--version": "1.3.10"}},
    )

    node_digest = fetch_manifest_digest(ocx.registry, nodejs.repo, nodejs.tag)
    bun_digest = fetch_manifest_digest(ocx.registry, bun.repo, bun.tag)
    node_dep = {"identifier": f"{nodejs.fq}@{node_digest}"}
    bun_dep = {"identifier": f"{bun.fq}@{bun_digest}"}

    webapp = make_package(
        ocx, f"{prefix}webapp", "2.0.0", tmp_path, bins=["serve"],
        env=webapp_env, dependencies=[node_dep, bun_dep],
        outputs={"serve": {"--version": "webapp 2.0.0"}},
    )

    return {
        "nodejs": [nodejs],
        "bun": [bun],
        "webapp": [webapp],
    }


def deps_visibility(ocx: OcxRunner, tmp_path: Path, prefix: str = "") -> dict[str, list[PackageInfo]]:
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

    path_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
    ]
    node_env = path_env + [
        {"key": "NODE_HOME", "type": "constant", "value": "${installPath}",
         "visibility": "public"},
    ]
    bun_env = path_env + [
        {"key": "BUN_HOME", "type": "constant", "value": "${installPath}",
         "visibility": "public"},
    ]

    # Leaf packages (no deps of their own)
    nodejs = make_package(
        ocx, f"{prefix}nodejs", "24.0.0", tmp_path, bins=["node"], env=node_env,
        outputs={"node": {"--version": "v24.14.0"}},
    )
    bun = make_package(
        ocx, f"{prefix}bun", "1.3.0", tmp_path, bins=["bun"], env=bun_env,
        outputs={"bun": {"--version": "1.3.10"}},
    )
    templates = make_package(
        ocx, f"{prefix}templates", "1.0.0", tmp_path, bins=["tpl"],
        outputs={"tpl": {"--version": "templates 1.0.0"}},
    )

    node_digest = fetch_manifest_digest(ocx.registry, nodejs.repo, nodejs.tag)
    bun_digest = fetch_manifest_digest(ocx.registry, bun.repo, bun.tag)
    templates_digest = fetch_manifest_digest(ocx.registry, templates.repo, templates.tag)

    # Intermediate packages (depend on nodejs)
    server = make_package(
        ocx, f"{prefix}server", "1.0.0", tmp_path, bins=["server"], env=path_env,
        dependencies=[{"identifier": f"{nodejs.fq}@{node_digest}", "visibility": "public"}],
        outputs={"server": {"--version": "server 1.0.0"}},
    )
    renderer = make_package(
        ocx, f"{prefix}renderer", "1.0.0", tmp_path, bins=["render"], env=path_env,
        dependencies=[{"identifier": f"{nodejs.fq}@{node_digest}", "visibility": "public"}],
        outputs={"render": {"--version": "renderer 1.0.0"}},
    )

    server_digest = fetch_manifest_digest(ocx.registry, server.repo, server.tag)
    renderer_digest = fetch_manifest_digest(ocx.registry, renderer.repo, renderer.tag)

    # Root: webapp depends on server (public), bun (public), renderer (private)
    webapp = make_package(
        ocx, f"{prefix}webapp", "2.0.0", tmp_path, bins=["serve"],
        env=path_env + [{"key": "APP_HOME", "type": "constant", "value": "${installPath}",
                          "visibility": "public"}],
        dependencies=[
            {"identifier": f"{server.fq}@{server_digest}", "visibility": "public"},
            {"identifier": f"{bun.fq}@{bun_digest}", "visibility": "public"},
            {"identifier": f"{renderer.fq}@{renderer_digest}", "visibility": "private"},
            {"identifier": f"{templates.fq}@{templates_digest}"},
        ],
        outputs={"serve": {"--version": "webapp 2.0.0"}},
    )

    # Pre-install so recordings don't need `ocx install` on screen.
    ocx.run("package", "install", "--select", webapp.short)

    return {
        "nodejs": [nodejs],
        "bun": [bun],
        "templates": [templates],
        "server": [server],
        "renderer": [renderer],
        "webapp": [webapp],
    }


def publisher(ocx: OcxRunner, tmp_path: Path, prefix: str = "") -> dict[str, list[PackageInfo]]:
    """Provision a publisher work directory under ``tmp_path``.

    Lays out three source trees plus a sidecar metadata file:

    - ``build/bin/mytool``   — package contents for v1 (and the second push of
      a multi-platform pair).
    - ``build-v2/bin/mytool`` — package contents for v1.0.1 in cascade and
      layer-reuse demos.
    - ``base/lib/data``      — shared base layer used by the layer-reuse demo.
    - ``metadata.json``      — single sidecar that all pushes reference.

    No package is published. The publisher recordings demonstrate
    ``ocx package create`` and ``ocx package push`` end-to-end on screen, so
    the setup deliberately leaves the registry empty for the script to
    populate.

    The returned mapping uses display name ``mytool`` so that the UUID-prefixed
    actual repository name (used when commands run) gets sanitised back to
    ``mytool`` in the rendered cast.
    """
    actual_repo = f"{prefix}mytool"

    # v1 source tree
    build = tmp_path / "build" / "bin"
    build.mkdir(parents=True)
    binary = build / "mytool"
    binary.write_text("#!/bin/sh\necho mytool 1.0.0\n")
    binary.chmod(binary.stat().st_mode | stat.S_IEXEC)

    # v2 source tree (cascade + layer-reuse)
    build_v2 = tmp_path / "build-v2" / "bin"
    build_v2.mkdir(parents=True)
    binary_v2 = build_v2 / "mytool"
    binary_v2.write_text("#!/bin/sh\necho mytool 1.0.1\n")
    binary_v2.chmod(binary_v2.stat().st_mode | stat.S_IEXEC)

    # Shared base layer (layer-reuse)
    base = tmp_path / "base" / "lib"
    base.mkdir(parents=True)
    # Deterministic content so the layer digest is stable across runs of the
    # same recording (small deduplicable file).
    (base / "data").write_text("shared base library\n")

    # Sidecar metadata
    metadata = {
        "type": "bundle",
        "version": 1,
        "env": [
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
                "visibility": "public",
            },
        ],
    }
    (tmp_path / "metadata.json").write_text(json.dumps(metadata, indent=2))

    # README + logo for the package describe demo
    (tmp_path / "README.md").write_text(
        "# mytool\n\nA small example tool used in the OCX authoring guides.\n"
    )

    # Stub PackageInfo: triggers display-name → actual-repo rewriting in
    # test_recordings.py. Tag is irrelevant — only ``repo`` is consulted.
    return {
        "mytool": [
            PackageInfo(
                repo=actual_repo,
                tag="display",
                short=f"{actual_repo}:display",
                fq=f"{ocx.registry}/{actual_repo}:display",
                content_dir=tmp_path,
                marker=f"publisher-{uuid4().hex[:8]}",
                platform=current_platform(),
            ),
        ],
    }


def patches_consumer(ocx: OcxRunner, tmp_path: Path, prefix: str = "") -> dict[str, list[PackageInfo]]:
    """Provision a working corporate-patches scenario for the consumer cast.

    Publishes ``cmake`` (the base tool) and an env-only companion ``corp-ca``
    that carries an INTERFACE ``SSL_CERT_FILE`` overlay, configures the
    ``[patches]`` tier, installs cmake, then publishes a per-base descriptor
    mapping cmake → corp-ca.

    The descriptor is published *after* the base install so lazy discovery
    records "no descriptor" at install time — the recorded ``ocx patch sync``
    is what actually discovers and installs the companion on screen, and
    ``ocx package env --show-patches`` then shows the overlaid ``SSL_CERT_FILE``.

    The ``[patches]`` registry is prefixed per provision (SP7) so concurrent
    xdist workers never collide on a shared descriptor location.
    """
    cmake_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
    ]
    # Companion env MUST be `interface` to surface onto a consuming base's view.
    companion_env = [
        {"key": "SSL_CERT_FILE", "type": "constant", "value": "/etc/ssl/certs/corp-ca.pem",
         "visibility": "interface"},
    ]
    cmake = make_package(
        ocx, f"{prefix}cmake", "4.2.0", tmp_path, bins=["cmake"], env=cmake_env,
        outputs={"cmake": {"--version": (
            "cmake version 4.2.3\n"
            "\n"
            "CMake suite maintained and supported by Kitware (kitware.com/cmake)."
        )}},
    )
    companion = make_package(
        ocx, f"{prefix}corp-ca", "1.0.0", tmp_path, bins=[], env=companion_env,
    )

    # Configure the [patches] tier in the data-dir config; the recorder shell
    # and any child ocx read $OCX_HOME/config.toml.  Prefix the patch registry
    # so the global-descriptor probe + per-base descriptors stay isolated per
    # worker on the shared registry:2.
    # Bare registry host (matches OCX_INSECURE_REGISTRIES → HTTP on registry:2).
    # Per-worker isolation comes from the prefixed base repo embedded in each
    # per-base descriptor path; no `--global` descriptor is published here, so
    # the shared `<host>/global` slot stays untouched.
    patch_registry = ocx.registry
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        "[patches]\n"
        f'registry = "{patch_registry}"\n'
        "required = true\n"
    )

    # Install the base BEFORE the descriptor exists so lazy discovery finds
    # nothing — the on-screen `ocx patch sync` is what installs the companion.
    ocx.run("package", "install", cmake.short, format=None)

    # Publish a per-base descriptor (cmake → corp-ca).  Per-base keeps the
    # descriptor path scoped to the prefixed base repo (no cross-worker clash).
    descriptor = tmp_path / "consumer-descriptor.json"
    descriptor.write_text(json.dumps({
        "version": 1,
        "rules": [{"match": "*", "packages": [companion.fq], "required": True}],
    }))
    ocx.run("patch", "publish", "--descriptor-file", str(descriptor), cmake.fq, format=None)

    return {"cmake": [cmake], "corp-ca": [companion]}


def patches_maintainer(ocx: OcxRunner, tmp_path: Path, prefix: str = "") -> dict[str, list[PackageInfo]]:
    """Provision the maintainer cast: author → test → publish → freeze.

    Publishes a base tool ``mytool`` and an env-only ``corp-ca`` companion
    (INTERFACE ``SSL_CERT_FILE``), configures the ``[patches]`` tier, and writes
    the ``descriptor.json`` the cast previews with ``ocx patch test``, publishes
    with ``ocx patch publish``, and pins with ``ocx --global patch freeze``.

    The descriptor lives in the work dir (``$SCENARIO_TMP``) so the recorded
    ``--descriptor-file descriptor.json`` resolves it; its companion reference
    is the prefixed fq, which the cast sanitiser rewrites back to ``corp-ca``.
    """
    base_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin",
         "visibility": "public"},
    ]
    companion_env = [
        {"key": "SSL_CERT_FILE", "type": "constant", "value": "/etc/ssl/certs/corp-ca.pem",
         "visibility": "interface"},
    ]
    mytool = make_package(
        ocx, f"{prefix}mytool", "1.0.0", tmp_path, bins=["mytool"], env=base_env,
        outputs={"mytool": {"--version": "mytool 1.0.0"}},
    )
    companion = make_package(
        ocx, f"{prefix}corp-ca", "1.0.0", tmp_path, bins=[], env=companion_env,
    )

    # Bare registry host (matches OCX_INSECURE_REGISTRIES → HTTP on registry:2).
    # Per-worker isolation comes from the prefixed base repo embedded in each
    # per-base descriptor path; no `--global` descriptor is published here, so
    # the shared `<host>/global` slot stays untouched.
    patch_registry = ocx.registry
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        "[patches]\n"
        f'registry = "{patch_registry}"\n'
        "required = true\n"
    )

    # The cast authors a descriptor; provide it in the work dir so the recorded
    # `ocx patch test/publish --descriptor-file descriptor.json` resolve it.
    descriptor = tmp_path / "descriptor.json"
    descriptor.write_text(json.dumps({
        "version": 1,
        "rules": [{"match": "*", "packages": [companion.fq], "required": True}],
    }, indent=2))

    return {"mytool": [mytool], "corp-ca": [companion]}


SetupFn = Callable[[OcxRunner, Path, str], dict[str, list[PackageInfo]]]

SETUPS: dict[str, SetupFn] = {
    "basic": basic,
    "multi-version": multi_version,
    "full-catalog": full_catalog,
    "variants": variants,
    "dependencies": dependencies,
    "deps-visibility": deps_visibility,
    "publisher": publisher,
    "patches-consumer": patches_consumer,
    "patches-maintainer": patches_maintainer,
}
