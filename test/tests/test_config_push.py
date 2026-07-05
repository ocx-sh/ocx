# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx config push`` (managed-config v2 publish leg).

Managed config v2 publishes the payload as an ORDINARY ocx package (content =
``config.toml``, tar+gzip layer, synthesized bundle metadata) — no custom
artifact type. These tests pin that wire shape and the operator-facing
validation/exit-code contract.

Design record: ``.claude/artifacts/adr_managed_config_tier.md`` (v2
amendment); plan ``.claude/state/plans/plan_managed_config_v2.md`` Phase 1.
"""

from __future__ import annotations

import json
import urllib.request
from pathlib import Path
from typing import Any

from src.helpers import push_managed_config
from src.registry import fetch_manifest_from_registry, push_raw_config_package
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_payload(tmp_path: Path, content: str, name: str = "corp-config.toml") -> Path:
    """Writes a payload file (deliberately NOT named ``config.toml`` by
    default — the push must stage it under the canonical name regardless)."""
    path = tmp_path / name
    path.write_text(content)
    return path


def _fetch_manifest_by_digest(registry: str, repo: str, digest: str) -> dict[str, Any]:
    """Fetches a manifest by digest reference (``fetch_manifest_from_registry``
    only accepts tags)."""
    url = f"http://{registry}/v2/{repo}/manifests/{digest}"
    request = urllib.request.Request(
        url,
        headers={"Accept": "application/vnd.oci.image.manifest.v1+json"},
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.loads(response.read())


def _list_tags(registry: str, repo: str) -> set[str]:
    try:
        with urllib.request.urlopen(f"http://{registry}/v2/{repo}/tags/list", timeout=5) as response:
            return set(json.loads(response.read()).get("tags") or [])
    except urllib.error.HTTPError as error:
        if error.code == 404:  # repository was never created
            return set()
        raise


# ---------------------------------------------------------------------------
# Wire shape
# ---------------------------------------------------------------------------


def test_config_push_produces_ordinary_package_manifest(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """`config push` produces an ordinary package: image index with a single
    `any/any` entry pointing at an image manifest whose one layer is tar+gzip.
    The report surfaces the index digest (operator TOFU signal)."""
    payload = _write_payload(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    report = ocx.json("config", "push", "-i", f"{unique_repo}:1.0.0", "--new", str(payload))
    assert report["status"] == "pushed"
    assert report["manifest_digest"].startswith("sha256:")
    assert report["cascade_tags_written"] == []

    index = fetch_manifest_from_registry(registry, unique_repo, "1.0.0")
    assert index["mediaType"] == "application/vnd.oci.image.index.v1+json"
    entries = index["manifests"]
    platforms = {
        f"{entry['platform']['os']}/{entry['platform']['architecture']}"
        for entry in entries
        if entry.get("platform")
    }
    assert platforms == {"any/any"}, f"default --platform must be any/any, got {platforms}"

    manifest = _fetch_manifest_by_digest(registry, unique_repo, entries[0]["digest"])
    layers = manifest["layers"]
    assert len(layers) == 1
    assert layers[0]["mediaType"] == "application/vnd.oci.image.layer.v1.tar+gzip"


# ---------------------------------------------------------------------------
# Validation (exit 78)
# ---------------------------------------------------------------------------


def test_config_push_rejects_managed_section_exit_78(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    payload = _write_payload(tmp_path, '[managed]\nsource = "corp.example.com/ocx-config:user"\n')
    result = ocx.run("config", "push", "-i", f"{unique_repo}:1.0.0", "--new", str(payload), check=False)
    assert result.returncode == 78, result.stderr
    assert "[managed]" in result.stderr


def test_config_push_rejects_invalid_toml_exit_78(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    payload = _write_payload(tmp_path, "not = [valid\n")
    result = ocx.run("config", "push", "-i", f"{unique_repo}:1.0.0", "--new", str(payload), check=False)
    assert result.returncode == 78, result.stderr


def test_config_push_rejects_oversize_payload_exit_78(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    payload = _write_payload(tmp_path, "# padding\n" * 7_000)  # ~70 KiB > 64 KiB cap
    result = ocx.run("config", "push", "-i", f"{unique_repo}:1.0.0", "--new", str(payload), check=False)
    assert result.returncode == 78, result.stderr


def test_config_push_rejected_payload_pushes_nothing(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """Validation happens before any registry write — a rejected payload must
    leave the repository absent."""
    payload = _write_payload(tmp_path, '[managed]\nsource = "x/y:z"\n')
    ocx.run("config", "push", "-i", f"{unique_repo}:1.0.0", "--new", str(payload), check=False)
    assert _list_tags(registry, unique_repo) == set()


# ---------------------------------------------------------------------------
# Cascade variants
# ---------------------------------------------------------------------------


def test_config_push_cascade_writes_variant_tags_without_latest(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """`user-1.4.2 --cascade` on an empty repo (--new) writes the rolling
    variant tags `user-1.4`, `user-1`, `user` — variant pushes are terminal at
    the bare variant tag, never `latest`."""
    payload = _write_payload(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    report = ocx.json(
        "config", "push", "-i", f"{unique_repo}:user-1.4.2", "--cascade", "--new", str(payload)
    )
    assert set(report["cascade_tags_written"]) == {"user-1.4", "user-1", "user"}

    tags = _list_tags(registry, unique_repo)
    assert {"user-1.4.2", "user-1.4", "user-1", "user"} <= tags
    assert "latest" not in tags, "variant cascade must never write latest"


def test_config_push_cascade_rollforward_updates_variant_tags(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """A newer version cascades the rolling tags forward to its digest."""
    payload_a = _write_payload(tmp_path, '[registry]\ndefault = "a.example.com"\n', name="a.toml")
    payload_b = _write_payload(tmp_path, '[registry]\ndefault = "b.example.com"\n', name="b.toml")

    ocx.json("config", "push", "-i", f"{unique_repo}:user-1.0.0", "--cascade", "--new", str(payload_a))
    report_b = ocx.json("config", "push", "-i", f"{unique_repo}:user-1.0.1", "--cascade", str(payload_b))
    assert set(report_b["cascade_tags_written"]) == {"user-1.0", "user-1", "user"}

    floating = fetch_manifest_from_registry(registry, unique_repo, "user")
    pinned = fetch_manifest_from_registry(registry, unique_repo, "user-1.0.1")
    assert floating == pinned, "floating variant tag must follow the newest version"


def test_config_push_cascade_without_new_on_missing_repo_exits_79(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """`config push --cascade` without `--new` against a repository that has no
    tags yet cannot list existing versions to roll the cascade forward. It
    fails closed with exit 79 (the tag-list 404 → NotFound via ListTagsFailed)
    and a hint to pass `--new` for a first publish; nothing is written."""
    payload = _write_payload(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    result = ocx.run("config", "push", "-i", f"{unique_repo}:1.0.0", "--cascade", str(payload), check=False)
    assert result.returncode == 79, (
        f"cascade without --new on an empty repo must exit 79, got {result.returncode}: {result.stderr}"
    )
    assert "--new" in result.stderr, f"the error must hint at passing --new: {result.stderr!r}"
    assert _list_tags(registry, unique_repo) == set(), "a failed first-publish cascade must write nothing"


# ---------------------------------------------------------------------------
# Round-trip: config push → config update (product path, both legs)
# ---------------------------------------------------------------------------


def test_config_push_then_update_round_trip(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """The full product path: publish via `config push`, adopt via env-only
    `config update`, and the tier merges on a later command. The snapshot's
    digest equals the push report's digest (drift-identity coherence)."""
    ref = f"{registry}/{unique_repo}:user"
    pushed_digest = push_managed_config(
        ocx, unique_repo, "user", '[registry]\ndefault = "round-trip.example"\n', tmp_path
    )

    update = ocx.json("config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref})
    assert update["status"] == "updated"
    assert update["digest"] == pushed_digest, "snapshot digest must equal the pushed index digest"

    snapshot = json.loads(
        (Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "snapshot.json").read_text()
    )
    assert snapshot["digest"] == pushed_digest
    assert snapshot["tag"] == "user", "snapshot v2 must carry the source tag"
    assert "round-trip.example" in snapshot["config"]


def test_config_update_package_without_config_toml_exits_65(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    """A package whose layer has no config.toml is malformed registry data."""
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(registry, unique_repo, "v1", b"not a config\n", entry_name="README.md")

    result = ocx.run("config", "update", check=False, env_overrides={"OCX_MANAGED_CONFIG": ref})
    assert result.returncode == 65, f"missing config.toml must exit 65, got {result.returncode}: {result.stderr}"


def test_config_update_ignores_extra_archive_entries(
    ocx: OcxRunner, unique_repo: str, registry: str
) -> None:
    """Extra entries in the package layer are ignored — only config.toml is
    consumed."""
    ref = f"{registry}/{unique_repo}:v1"
    push_raw_config_package(
        registry,
        unique_repo,
        "v1",
        b'[registry]\ndefault = "extras-ignored.example"\n',
        extra_entries={"README.md": b"docs\n", "scripts/setup.sh": b"#!/bin/sh\n"},
    )

    update = ocx.json("config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref})
    assert update["status"] == "updated"
    snapshot = json.loads(
        (Path(ocx.env["OCX_HOME"]) / "state" / "managed-config" / "snapshot.json").read_text()
    )
    assert "extras-ignored.example" in snapshot["config"]
