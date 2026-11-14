#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Assemble the GitHub release assets from the stamped Bazel binaries.

    scripts/release_assets.py build --dist DIR --version V --sha SHA [--bin-root DIR] [--root DIR]

Reads `<bin-root>/<triple>/ocx[.exe]` (default `target/cross-release`, written by
`task cross:build:release`) for the eight shipped triples and writes into DIST the asset set
v0.6.6 shipped under cargo-dist, so installers and scripts keep resolving: one archive per
triple with its `.sha256`, `sha256.sum`, `dist-manifest.json` and `ocx.cdx.xml`, plus one
`ocx-<asset-target>.cdx.json` per triple (the per-platform attestation predicate).

Archives, checksums and manifest entries are named for the triple the binary was built for, so
Windows is `*-pc-windows-gnullvm`; v0.6.6 and earlier shipped it as `*-pc-windows-msvc`.

Exit codes: 0 written, 1 a binary is missing or the SBOM tool failed.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import shutil
import subprocess
import sys
import tarfile
import zipfile
from collections.abc import Callable
from pathlib import Path

# The shipped triples: Bazel build triple, archive name and manifest triple are one string.
TARGETS: tuple[str, ...] = (
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-gnullvm",
    "aarch64-pc-windows-gnullvm",
)

# Files copied from the repository root into every archive, as cargo-dist did.
EXTRA_FILES = ("CHANGELOG.md", "LICENSE", "LICENSE-THIRD-PARTY.md", "README.md")
CDX_MANIFEST = Path("crates/ocx_cli/Cargo.toml")

# (fmt, target-or-None, output-stem) -> the file the tool wrote.
SbomRunner = Callable[[Path, str, str | None, str], Path]


def cargo_cyclonedx(root: Path, fmt: str, target: str | None, stem: str) -> Path:
    """Run `cargo cyclonedx` once; it appends `.<fmt>` to the override, so `<stem>.cdx` yields `<stem>.cdx.<fmt>`."""
    cmd = [
        "cargo",
        "cyclonedx",
        "--format",
        fmt,
        "--manifest-path",
        str(root / CDX_MANIFEST),
    ]
    if target:
        cmd += ["--target", target]
    cmd += ["--override-filename", f"{stem}.cdx"]
    subprocess.run(cmd, cwd=root, check=True)
    return root / CDX_MANIFEST.parent / f"{stem}.cdx.{fmt}"


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _is_windows(asset_target: str) -> bool:
    return "windows" in asset_target


def _write_tar(archive: Path, inner: str, files: dict[str, Path]) -> None:
    with (
        archive.open("wb") as raw,
        gzip.GzipFile(fileobj=raw, mode="wb", mtime=0) as gz,
        tarfile.open(fileobj=gz, mode="w") as tar,
    ):
        top = tarfile.TarInfo(inner + "/")
        top.type, top.mode = tarfile.DIRTYPE, 0o755
        tar.addfile(top)
        for name, src in sorted(files.items()):
            data = src.read_bytes()
            info = tarfile.TarInfo(f"{inner}/{name}")
            info.size, info.mode = len(data), 0o755 if name == "ocx" else 0o644
            tar.addfile(info, io.BytesIO(data))


def _write_zip(archive: Path, files: dict[str, Path]) -> None:
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
        for name, src in sorted(files.items()):
            zf.writestr(
                zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0)),
                src.read_bytes(),
                zipfile.ZIP_DEFLATED,
            )


def build(
    dist: Path,
    version: str,
    sha: str,
    bin_root: Path,
    root: Path,
    sbom: SbomRunner = cargo_cyclonedx,
) -> list[str]:
    """Write the asset set into `dist`; return its file names, sorted."""
    missing = [
        str(bin_root / t / ("ocx.exe" if _is_windows(t) else "ocx"))
        for t in TARGETS
        if not (bin_root / t / ("ocx.exe" if _is_windows(t) else "ocx")).is_file()
    ]
    if missing:
        raise FileNotFoundError(f"missing binaries: {missing}")
    dist.mkdir(parents=True, exist_ok=True)

    artifacts: dict[str, dict[str, object]] = {}
    sums: list[str] = []
    for asset in TARGETS:
        exe = "ocx.exe" if _is_windows(asset) else "ocx"
        files = {exe: bin_root / asset / exe} | {
            n: root / n for n in EXTRA_FILES
        }
        archive = dist / (
            f"ocx-{asset}.zip" if _is_windows(asset) else f"ocx-{asset}.tar.gz"
        )
        if _is_windows(asset):
            _write_zip(archive, files)
        else:
            _write_tar(archive, f"ocx-{asset}", files)
        digest = _sha256(archive)
        line = f"{digest} *{archive.name}\n\n"
        (dist / f"{archive.name}.sha256").write_text(line, encoding="utf-8")
        sums.append(f"{digest} *{archive.name}\n")
        kinds = {
            "CHANGELOG.md": "changelog",
            "LICENSE": "license",
            "LICENSE-THIRD-PARTY.md": "license",
            "README.md": "readme",
        }
        artifacts[archive.name] = {
            "name": archive.name,
            "kind": "executable-zip",
            "target_triples": [asset],
            "assets": [{"name": n, "path": n, "kind": kinds[n]} for n in EXTRA_FILES]
            + [
                {
                    "id": f"ocx-{asset}-exe-ocx",
                    "name": "ocx",
                    "path": exe,
                    "kind": "executable",
                }
            ],
            "checksum": f"{archive.name}.sha256",
            "checksums": {"sha256": digest},
        }
        artifacts[f"{archive.name}.sha256"] = {
            "name": f"{archive.name}.sha256",
            "kind": "checksum",
            "target_triples": [asset],
        }
        shutil.move(
            sbom(root, "json", asset, f"ocx-{asset}"),
            dist / f"ocx-{asset}.cdx.json",
        )

    (dist / "sha256.sum").write_text(
        "".join(sorted(sums, key=lambda s: s.split("*")[1])) + "\n", encoding="utf-8"
    )
    shutil.move(sbom(root, "xml", None, "ocx"), dist / "ocx.cdx.xml")
    artifacts["ocx.cdx.xml"] = {"name": "ocx.cdx.xml", "kind": "sbom"}
    artifacts["sha256.sum"] = {"name": "sha256.sum", "kind": "unified-checksum"}

    manifest = {
        "announcement_tag": f"v{version}",
        "announcement_is_prerelease": False,
        "commit": sha,
        "releases": [
            {
                "app_name": "ocx",
                "app_version": version,
                "display_name": "ocx",
                "display": True,
                "artifacts": sorted(artifacts),
            }
        ],
        "artifacts": dict(sorted(artifacts.items())),
    }
    (dist / "dist-manifest.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )
    return sorted(p.name for p in dist.iterdir())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = parser.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build", help="write the release asset set")
    b.add_argument("--dist", type=Path, required=True)
    b.add_argument("--version", required=True)
    b.add_argument("--sha", required=True)
    b.add_argument("--bin-root", type=Path, default=Path("target/cross-release"))
    b.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args(argv)
    try:
        names = build(
            args.dist,
            args.version.removeprefix("v"),
            args.sha,
            args.bin_root,
            args.root,
        )
    except (FileNotFoundError, subprocess.CalledProcessError) as err:
        print(f"release_assets: {err}", file=sys.stderr)
        return 1
    print("\n".join(names))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
