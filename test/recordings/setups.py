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
    return {
        "hello-world": [
            make_package(ocx, "hello-world", "1.0.0", tmp_path, size_mb=SIZE_MB),
        ],
    }


def multi_version(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """One package with multiple versions."""
    return {
        "python": [
            make_package(ocx, "python", "3.12.0", tmp_path, size_mb=SIZE_MB, bins=["python"]),
            make_package(ocx, "python", "3.11.0", tmp_path, size_mb=SIZE_MB, bins=["python"], new=False),
        ],
    }


def full_catalog(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """Multiple packages for index/catalog demos."""
    return {
        "hello-world": [
            make_package(ocx, "hello-world", "1.0.0", tmp_path, size_mb=SIZE_MB),
        ],
        "cmake": [
            make_package(ocx, "cmake", "3.28.0", tmp_path, size_mb=SIZE_MB, bins=["cmake"]),
        ],
        "python": [
            make_package(ocx, "python", "3.12.0", tmp_path, size_mb=SIZE_MB, bins=["python"]),
            make_package(ocx, "python", "3.11.0", tmp_path, size_mb=SIZE_MB, bins=["python"], new=False),
        ],
        "node": [
            make_package(ocx, "node", "22.0.0", tmp_path, size_mb=SIZE_MB, bins=["node"]),
        ],
        "clang": [
            make_package(ocx, "clang", "18.0.0", tmp_path, size_mb=SIZE_MB, bins=["clang", "clang++"]),
        ],
    }


def plantuml(ocx: OcxRunner, tmp_path: Path) -> dict[str, list[PackageInfo]]:
    """Java runtime + PlantUML — realistic multi-package composition demo."""
    java_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "JAVA_HOME", "type": "constant", "value": "${installPath}"},
    ]
    plantuml_env = [
        {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        {"key": "PLANTUML_JAR", "type": "constant", "value": "${installPath}/lib/plantuml.jar"},
    ]
    return {
        "java": [
            make_package(ocx, "java", "21.0.0", tmp_path, size_mb=SIZE_MB, bins=["java", "javac"], env=java_env),
        ],
        "plantuml": [
            make_package(ocx, "plantuml", "1.0.0", tmp_path, size_mb=SIZE_MB, bins=["plantuml"], env=plantuml_env),
        ],
    }


SETUPS: dict[str, Callable] = {
    "basic": basic,
    "multi-version": multi_version,
    "full-catalog": full_catalog,
    "plantuml": plantuml,
}
