# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Helpers the telemetry acceptance modules share."""

from __future__ import annotations

from pathlib import Path

from src.helpers import make_package
from src.otlp_fixture import OtlpCollector
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo


def armed(collector: OtlpCollector, **extra: str) -> dict[str, str]:
    """Environment that arms telemetry and aims it at ``collector``."""
    return {
        "OCX_TELEMETRY": "1",
        "OTEL_EXPORTER_OTLP_ENDPOINT": collector.endpoint,
        **extra,
    }


def write_config(ocx: OcxRunner, body: str) -> Path:
    """Write ``body`` as the isolated home's ``config.toml``."""
    path = ocx.ocx_home / "config.toml"
    path.write_text(body, encoding="utf-8")
    return path


def package_with_dependency(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> tuple[PackageInfo, PackageInfo]:
    """Publish a root package with one public dependency; returns ``(root, dep)``."""
    dep = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path)
    digest = fetch_platform_manifest_digest(ocx.registry, dep.repo, dep.tag)
    root = make_package(
        ocx,
        f"{unique_repo}_root",
        "1.0.0",
        tmp_path,
        dependencies=[{"identifier": f"{dep.fq}@{digest}", "visibility": "public"}],
    )
    return root, dep
