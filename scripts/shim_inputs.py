#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The digest of every build input of the committed launcher blobs that is not shim source.

    python3 scripts/shim_inputs.py [--root DIR] (--print | --check)

The blobs are not byte-reproducible, so freshness is judged on inputs. `build-windows-shims.yml` already reds
a blob older than the last change under `crates/ocx_shim/` or `crates/ocx_env_apply/`; this covers the rest:
the toolchain file, the `shim` profile, the workspace entries of the launcher's direct dependencies, the
registry packages its lock closure resolves to, the `shim:build` command line, and the Zig and cargo-zigbuild
pins of the workflow that cuts the blobs. Workspace-member versions are left out, so a release bump is not a
change. `--check` compares the digest with `crates/ocx_store/src/shim_inputs.sha256`, which a blob re-cut
rewrites with `--print`.

Exit: 0 the digest matches (or was printed); 1 it does not; 2 an input is missing or reads as nothing.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tomllib
from pathlib import Path

RECORDED = Path("crates/ocx_store/src/shim_inputs.sha256")
WORKFLOW = Path(".github/workflows/build-windows-shims.yml")
TASKFILE = Path("taskfiles/rust.taskfile.yml")
LAUNCHER_CRATES = ("ocx_shim", "ocx_env_apply")


class InputError(Exception):
    """An input the digest needs is absent or read as empty."""


def _one(pattern: str, text: str, where: Path) -> str:
    found = re.findall(pattern, text, flags=re.MULTILINE)
    if len(found) != 1:
        raise InputError(
            f"{where}: expected one match of {pattern!r}, found {len(found)}"
        )
    return found[0]


def _lock_closure(lock: dict) -> list[dict[str, str]]:
    packages = lock.get("package", [])
    by_name: dict[str, list[dict]] = {}
    for package in packages:
        by_name.setdefault(package["name"], []).append(package)

    def resolve(spec: str) -> dict:
        name, _, version = spec.partition(" ")
        candidates = [
            p for p in by_name.get(name, []) if not version or p["version"] == version
        ]
        if len(candidates) != 1:
            raise InputError(
                f"Cargo.lock: dependency {spec!r} resolves to {len(candidates)} packages"
            )
        return candidates[0]

    seen: dict[tuple[str, str], dict] = {}
    stack = [resolve(name) for name in LAUNCHER_CRATES]
    while stack:
        package = stack.pop()
        key = (package["name"], package["version"])
        if key in seen:
            continue
        seen[key] = package
        stack.extend(resolve(spec) for spec in package.get("dependencies", []))
    # Only registry packages: a workspace member's version is the release number, not a build input.
    registry = [
        {"name": p["name"], "version": p["version"], "checksum": p.get("checksum", "")}
        for p in seen.values()
        if "source" in p
    ]
    if not any(entry["name"] == "serde_json" for entry in registry):
        raise InputError(
            "Cargo.lock: the launcher's closure holds no serde_json; the walk read nothing"
        )
    return sorted(registry, key=lambda entry: (entry["name"], entry["version"]))


def inputs(root: Path) -> dict[str, object]:
    """Every non-source input, read from the tree at `root`; raises `InputError` on a missing or empty one."""
    try:
        manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        toolchain = tomllib.loads(
            (root / "rust-toolchain.toml").read_text(encoding="utf-8")
        )
        lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
        workflow = (root / WORKFLOW).read_text(encoding="utf-8")
        taskfile = (root / TASKFILE).read_text(encoding="utf-8")
        direct = set()
        for crate in LAUNCHER_CRATES:
            crate_manifest = tomllib.loads(
                (root / "crates" / crate / "Cargo.toml").read_text(encoding="utf-8")
            )
            direct.update(crate_manifest.get("dependencies", {}))
            for table in crate_manifest.get("target", {}).values():
                direct.update(table.get("dependencies", {}))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise InputError(str(error)) from error

    profile = manifest.get("profile", {}).get("shim")
    if not profile:
        raise InputError("Cargo.toml: no [profile.shim] table")
    workspace_deps = manifest.get("workspace", {}).get("dependencies", {})
    external = sorted(name for name in direct if name not in LAUNCHER_CRATES)
    missing = [name for name in external if name not in workspace_deps]
    if not external or missing:
        raise InputError(
            f"Cargo.toml: launcher dependencies {external}, without a workspace entry {missing}"
        )
    return {
        "toolchain": toolchain.get("toolchain") or {},
        "profile": profile,
        "dependencies": {name: workspace_deps[name] for name in external},
        "lock": _lock_closure(lock),
        "zig": _one(r'^\s*ZIG_VERSION: "([^"]+)"', workflow, WORKFLOW),
        "zig_sha256": _one(r'^\s*ZIG_SHA256: "([0-9a-f]{64})"', workflow, WORKFLOW),
        "cargo_zigbuild": _one(r"^\s*tool: cargo-zigbuild@(\S+)", workflow, WORKFLOW),
        "build_command": _one(
            r"^\s*(RUSTFLAGS=.*\n\s*cargo zigbuild .*)$", taskfile, TASKFILE
        ),
    }


def digest(root: Path) -> str:
    canonical = json.dumps(inputs(root), sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path.cwd())
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--print", action="store_true", help="print the digest")
    mode.add_argument(
        "--check", action="store_true", help="compare it with the recorded one"
    )
    args = parser.parse_args(argv)
    try:
        current = digest(args.root)
        if args.print:
            print(current)
            return 0
        recorded = (args.root / RECORDED).read_text(encoding="utf-8").strip()
    except (InputError, OSError) as error:
        print(f"shim_inputs: {error}", file=sys.stderr)
        return 2
    if current == recorded:
        print(f"shim_inputs: launcher build inputs match the recorded digest {current}")
        return 0
    print(
        f"shim_inputs: launcher build inputs changed since the blobs were cut ({RECORDED} holds {recorded}, "
        f"the tree is {current}). Re-cut every blob from this run's ocx-shim-fresh-<target> artifacts, update "
        f"RECORDED_SHA256 in crates/ocx_store/src/shim.rs and write `python3 scripts/shim_inputs.py --print` "
        f"to {RECORDED}.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
