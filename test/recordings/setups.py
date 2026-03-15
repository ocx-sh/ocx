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

SIZE_MB = 5


def basic(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """Single package, one version."""
    uv_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
    ]
    return {
        "uv": [
            make_package(ocx, "uv", "0.10.0", tmp_path, size_mb=SIZE_MB, bins=["uv"], env=uv_env),
        ],
    }


def multi_version(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """One package with multiple versions."""
    python_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "PYTHONPATH", "type": "constant", "value": "${installPath}/lib/python3.12"},
    ]
    return {
        "python": [
            make_package(ocx, "python", "3.12.0", tmp_path, size_mb=SIZE_MB, bins=["python"], env=python_env),
            make_package(ocx, "python", "3.11.0", tmp_path, size_mb=SIZE_MB, bins=["python"], env=python_env, new=False),
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
    python_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "PYTHONPATH", "type": "constant", "value": "${installPath}/lib/python3.12"},
    ]
    node_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
    ]
    bun_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
    ]
    llvm_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
    ]
    java_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "JAVA_HOME", "type": "constant", "value": "${installPath}"},
    ]
    return {
        "uv": [
            make_package(ocx, "uv", "0.10.0", tmp_path, size_mb=SIZE_MB, bins=["uv"], env=uv_env),
        ],
        "cmake": [
            make_package(ocx, "cmake", "3.31.0", tmp_path, size_mb=SIZE_MB, bins=["cmake"], env=cmake_env),
        ],
        "python": [
            make_package(ocx, "python", "3.12.0", tmp_path, size_mb=SIZE_MB, bins=["python"], env=python_env),
            make_package(ocx, "python", "3.11.0", tmp_path, size_mb=SIZE_MB, bins=["python"], env=python_env, new=False),
        ],
        "node": [
            make_package(ocx, "node", "22.0.0", tmp_path, size_mb=SIZE_MB, bins=["node"], env=node_env),
        ],
        "bun": [
            make_package(ocx, "bun", "1.2.0", tmp_path, size_mb=SIZE_MB, bins=["bun"], env=bun_env),
        ],
        "llvm": [
            make_package(ocx, "llvm", "22.1.0", tmp_path, size_mb=SIZE_MB, bins=["clang", "clang++"], env=llvm_env),
        ],
        "java": [
            make_package(ocx, "java", "21.0.0", tmp_path, size_mb=SIZE_MB, bins=["java", "javac"], env=java_env),
        ],
    }


SETUPS: dict[str, Callable] = {
    "basic": basic,
    "multi-version": multi_version,
    "full-catalog": full_catalog,
}
