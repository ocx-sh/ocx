#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Deterministic fixture generator: tiny handcrafted wheels + pylock.toml
fixtures for `ocx_python` unit tests.

Re-running this script must reproduce every output file byte-for-byte:
wheels are built with a fixed DOS-epoch zip mtime, `ZIP_STORED` (no
compression, so no zlib-version drift), sorted entries, and a fixed
unix mode/owner per entry regardless of the build host. This mirrors the
determinism contract `repack` (W1.6) applies to real wheels.

Run with no arguments to (re)write the checked-in fixtures. `--out DIR`
writes elsewhere (used by the determinism self-check below).
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import io
import zipfile
from pathlib import Path

FIXTURES_DIR = Path(__file__).parent

# DOS zip epoch floor (1980-01-01) - the earliest date a zip can express, and
# the same value every run/machine produces.
ZIP_EPOCH = (1980, 1, 1, 0, 0, 0)


def _record_hash(data: bytes) -> str:
    """PEP 376 RECORD hash field: `sha256=<url-safe base64, no padding>`."""
    digest = hashlib.sha256(data).digest()
    return "sha256=" + base64.urlsafe_b64encode(digest).rstrip(b"=").decode("ascii")


def build_wheel(
    dist_escaped: str,
    version: str,
    wheel_tags: list[str],
    files: dict[str, bytes],
    *,
    filename_tag: str | None = None,
    name: str | None = None,
    root_is_purelib: bool = True,
    executable: frozenset[str] = frozenset(),
) -> tuple[str, bytes]:
    """Assembles one deterministic wheel zip.

    `files` maps archive path -> content for everything but the
    auto-generated `METADATA`/`WHEEL`/`RECORD` dist-info trio; callers add
    `entry_points.txt` and any `.data/...` paths directly into `files`.

    `wheel_tags` are the WHEEL-file `Tag:` lines (one per concrete
    python-abi-platform triple — real wheels explode a compressed filename
    tag like `py2.py3-none-any` into two `Tag:` lines). `filename_tag`
    defaults to `wheel_tags[0]`; pass it explicitly for a compressed
    multi-tag filename.

    Returns `(filename, wheel_bytes)`.
    """
    dist_info = f"{dist_escaped}-{version}.dist-info"
    files = dict(files)
    files.setdefault(
        f"{dist_info}/METADATA",
        f"Metadata-Version: 2.1\nName: {name or dist_escaped}\nVersion: {version}\n".encode(),
    )
    tag_lines = "".join(f"Tag: {tag}\n" for tag in wheel_tags)
    files.setdefault(
        f"{dist_info}/WHEEL",
        (
            "Wheel-Version: 1.0\n"
            "Generator: ocx_python-fixtures\n"
            f"Root-Is-Purelib: {'true' if root_is_purelib else 'false'}\n"
            f"{tag_lines}"
        ).encode(),
    )

    record_lines = [f"{path},{_record_hash(data)},{len(data)}" for path, data in sorted(files.items())]
    record_lines.append(f"{dist_info}/RECORD,,")
    files[f"{dist_info}/RECORD"] = ("\n".join(record_lines) + "\n").encode()

    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_STORED) as archive:
        for path in sorted(files):
            info = zipfile.ZipInfo(path, date_time=ZIP_EPOCH)
            mode = 0o755 if path in executable else 0o644
            info.external_attr = mode << 16
            info.create_system = 3  # unix - fixed regardless of build host
            archive.writestr(info, files[path])

    resolved_tag = filename_tag or wheel_tags[0]
    filename = f"{dist_escaped}-{version}-{resolved_tag}.whl"
    return filename, buffer.getvalue()


# ── nspkg.pth boilerplate (legacy pre-PEP-420 namespace declaration) ────────

_NSPKG_PTH_CONTENT = (
    "import sys, types, os;"
    "has_mfs = sys.version_info > (3, 5);"
    "p = os.path.join(*(sys.path[0], *('legacy_ns',)));"
    "importlib = has_mfs and __import__('importlib.util');"
    "has_mfs and __import__('importlib.machinery');"
    "m = has_mfs and sys.modules.setdefault("
    "'legacy_ns', importlib.util.module_from_spec("
    "importlib.machinery.PathFinder.find_spec('legacy_ns', [os.path.dirname(p)])));"
    "m = m or sys.modules.setdefault('legacy_ns', types.ModuleType('legacy_ns'));"
    "mp = (m or []) and m.__dict__.setdefault('__path__', []);"
    "(p not in mp) and mp.append(p)\n"
)


def build_all_wheels() -> dict[str, bytes]:
    """Builds every fixture wheel. Returns `{filename: wheel_bytes}`."""
    wheels: dict[str, bytes] = {}

    def add(filename: str, data: bytes) -> None:
        wheels[filename] = data

    # 1. Pure-python (py3-none-any).
    add(
        *build_wheel(
            "purelib_pkg",
            "1.0.0",
            ["py3-none-any"],
            {
                "purelib_pkg/__init__.py": b"__version__ = '1.0.0'\n",
                "purelib_pkg/core.py": b"def greet():\n    return 'hello from purelib_pkg'\n",
            },
        )
    )

    # 2. cpXY C-ext stub (cp312-cp312-linux_x86_64).
    add(
        *build_wheel(
            "cext_pkg",
            "2.0.0",
            ["cp312-cp312-linux_x86_64"],
            {
                "cext_pkg/__init__.py": b"from cext_pkg._native import compute\n",
                "cext_pkg/_native.cpython-312-x86_64-linux-gnu.so": (
                    b"\x7fELF" + b"\x00" * 12 + b"ocx_python fixture stub - not a real ELF binary\n"
                ),
            },
            root_is_purelib=False,
        )
    )

    # 3. abi3 (cp312-abi3-manylinux_2_28_x86_64).
    add(
        *build_wheel(
            "abi3_pkg",
            "3.1.0",
            ["cp312-abi3-manylinux_2_28_x86_64"],
            {
                "abi3_pkg/__init__.py": b"from abi3_pkg._native import compute\n",
                "abi3_pkg/_native.abi3.so": (
                    b"\x7fELF" + b"\x00" * 12 + b"ocx_python fixture stub - abi3, not a real ELF binary\n"
                ),
            },
            root_is_purelib=False,
        )
    )

    # 4/5. PEP 420 namespace pair: both under google/cloud/, distinct leaves,
    # no __init__.py in the shared namespace dirs (implicit namespace).
    add(
        *build_wheel(
            "google_cloud_foo",
            "1.0.0",
            ["py3-none-any"],
            {
                "google/cloud/foo/__init__.py": b"",
                "google/cloud/foo/client.py": b"def client():\n    return 'foo client'\n",
            },
            name="google-cloud-foo",
        )
    )
    add(
        *build_wheel(
            "google_cloud_bar",
            "1.0.0",
            ["py3-none-any"],
            {
                "google/cloud/bar/__init__.py": b"",
                "google/cloud/bar/client.py": b"def client():\n    return 'bar client'\n",
            },
            name="google-cloud-bar",
        )
    )

    # 6. .data/{scripts,data}.
    add(
        *build_wheel(
            "data_pkg",
            "1.0.0",
            ["py3-none-any"],
            {
                "data_pkg/__init__.py": b"__version__ = '1.0.0'\n",
                "data_pkg-1.0.0.data/scripts/data_pkg-launcher": (
                    b"#!python\nimport data_pkg\nprint('launched')\n"
                ),
                "data_pkg-1.0.0.data/data/share/data_pkg/config.json": b'{"greeting": "hi"}\n',
            },
            executable=frozenset({"data_pkg-1.0.0.data/scripts/data_pkg-launcher"}),
        )
    )

    # 7. console_scripts incl. extras-gated script + dotted-attr reference.
    add(
        *build_wheel(
            "console_pkg",
            "1.0.0",
            ["py3-none-any"],
            {
                "console_pkg/__init__.py": b"",
                "console_pkg/mod.py": (
                    "class Class:\n"
                    "    @staticmethod\n"
                    "    def method():\n"
                    "        return 'dotted-attr entrypoint'\n"
                    "\n\n"
                    "def main():\n"
                    "    print('console_pkg main')\n"
                ).encode(),
                "console_pkg-1.0.0.dist-info/entry_points.txt": (
                    "[console_scripts]\n"
                    "console-pkg = console_pkg:main\n"
                    "blackd = blackd:main [d]\n"
                    "foo = console_pkg.mod:Class.method\n"
                ).encode(),
            },
        )
    )

    # 8. Legacy nspkg.pth (pre-PEP-420 namespace declaration).
    add(
        *build_wheel(
            "legacy_ns_pkg",
            "1.0.0",
            ["py3-none-any"],
            {
                "legacy_ns_pkg-nspkg.pth": _NSPKG_PTH_CONTENT.encode(),
                "legacy_ns/pkg/__init__.py": b"",
            },
            name="legacy-ns-pkg",
        )
    )

    # 9. py2.py3-none-any (universal wheel; WHEEL exploded to two Tag: lines).
    add(
        *build_wheel(
            "universal_pkg",
            "1.0.0",
            ["py2-none-any", "py3-none-any"],
            {"universal_pkg/__init__.py": b"__version__ = '1.0.0'\n"},
            filename_tag="py2.py3-none-any",
        )
    )

    return wheels


# ── pylock.toml fixtures ─────────────────────────────────────────────────

def _wheel_block(filename: str, sha256_hex: str | None) -> str:
    lines = [
        "[[packages.wheels]]",
        f'name = "{filename}"',
        f'url = "{{ASSET_BASE}}/{filename}"',
    ]
    if sha256_hex is not None:
        lines.append(f'hashes = {{ sha256 = "{sha256_hex}" }}')
    return "\n".join(lines) + "\n"


def _fake_hash(seed: str) -> str:
    """Deterministic, realistic-looking sha256 hex for wheels the lock
    fixtures reference but that this generator does not build bytes for
    (marker-fork / environments fixtures test lock parsing, not download)."""
    return hashlib.sha256(seed.encode()).hexdigest()


def build_pylock_fixtures(wheel_hashes: dict[str, str]) -> dict[str, str]:
    """Builds every pylock.toml fixture. Returns `{relative_path: content}`."""
    locks: dict[str, str] = {}

    # Valid multi-package lock: every generated wheel above, real hashes.
    packages = []
    for filename, sha256_hex in sorted(wheel_hashes.items()):
        # filename = "<dist>-<version>-<tag>.whl" (tag may itself embed hyphens)
        dist_escaped, version = filename.split("-")[0], filename.split("-")[1]
        packages.append(
            "[[packages]]\n"
            f'name = "{dist_escaped.replace("_", "-")}"\n'
            f'version = "{version}"\n\n' + _wheel_block(filename, sha256_hex)
        )
    locks["valid-multi.toml"] = (
        'lock-version = "1.0"\n'
        'created-by = "ocx_python fixture generator"\n'
        'requires-python = ">=3.9"\n\n' + "\n".join(packages)
    )

    # sdist-only package: no `wheels` key at all -> LockError::SdistOnly.
    locks["sdist-only.toml"] = (
        'lock-version = "1.0"\n'
        'created-by = "ocx_python fixture generator"\n\n'
        "[[packages]]\n"
        'name = "legacy-toolkit"\n'
        'version = "1.0.0"\n\n'
        "[packages.sdist]\n"
        'url = "{ASSET_BASE}/legacy-toolkit-1.0.0.tar.gz"\n'
        f'hashes = {{ sha256 = "{_fake_hash("legacy-toolkit-1.0.0.tar.gz")}" }}\n'
    )

    # Missing-hash wheel: wheel table present, no `hashes` key -> LockError::MissingHash.
    locks["missing-hash.toml"] = (
        'lock-version = "1.0"\n'
        'created-by = "ocx_python fixture generator"\n\n'
        "[[packages]]\n"
        'name = "nohash-pkg"\n'
        'version = "1.0.0"\n\n' + _wheel_block("nohash_pkg-1.0.0-py3-none-any.whl", None)
    )

    # Marker forks: OS fork (colorama) + inverted OS fork (watchdog).
    locks["marker-forks.toml"] = (
        'lock-version = "1.0"\n'
        'created-by = "ocx_python fixture generator"\n'
        'requires-python = ">=3.9"\n\n'
        "[[packages]]\n"
        'name = "colorama"\n'
        'version = "0.4.6"\n'
        'marker = "sys_platform == \\"win32\\""\n\n'
        + _wheel_block("colorama-0.4.6-py2.py3-none-any.whl", _fake_hash("colorama-0.4.6"))
        + "\n"
        "[[packages]]\n"
        'name = "watchdog"\n'
        'version = "4.0.0"\n'
        'marker = "platform_system != \\"Darwin\\""\n\n'
        + _wheel_block("watchdog-4.0.0-py3-none-any.whl", _fake_hash("watchdog-4.0.0"))
    )

    # `environments` key: lock scoped to a single target environment.
    locks["environments.toml"] = (
        'lock-version = "1.0"\n'
        'created-by = "ocx_python fixture generator"\n'
        'requires-python = ">=3.9"\n'
        'environments = ["sys_platform == \\"linux\\" and platform_machine == \\"x86_64\\""]\n\n'
        "[[packages]]\n"
        'name = "onlylinux-pkg"\n'
        'version = "1.0.0"\n\n'
        + _wheel_block("onlylinux_pkg-1.0.0-py3-none-any.whl", _fake_hash("onlylinux-pkg-1.0.0"))
    )

    return locks


def self_check(filename: str, data: bytes) -> None:
    """Smallest runnable check: every generated wheel is a valid, readable zip
    carrying its dist-info trio."""
    buffer = io.BytesIO(data)
    assert zipfile.is_zipfile(buffer), f"{filename} is not a valid zip"
    with zipfile.ZipFile(buffer) as archive:
        names = archive.namelist()
        assert any(name.endswith(".dist-info/METADATA") for name in names), f"{filename} missing METADATA"
        assert any(name.endswith(".dist-info/WHEEL") for name in names), f"{filename} missing WHEEL"
        assert any(name.endswith(".dist-info/RECORD") for name in names), f"{filename} missing RECORD"
        assert names == sorted(names), f"{filename} zip entries are not sorted"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=FIXTURES_DIR, help="output directory (default: this directory)")
    args = parser.parse_args()

    wheels_dir = args.out / "wheels"
    pylock_dir = args.out / "pylock"
    wheels_dir.mkdir(parents=True, exist_ok=True)
    pylock_dir.mkdir(parents=True, exist_ok=True)

    wheels = build_all_wheels()
    wheel_hashes = {}
    for filename, data in wheels.items():
        self_check(filename, data)
        (wheels_dir / filename).write_bytes(data)
        wheel_hashes[filename] = hashlib.sha256(data).hexdigest()

    locks = build_pylock_fixtures(wheel_hashes)
    for relative_path, content in locks.items():
        (pylock_dir / relative_path).write_text(content)

    print(f"wrote {len(wheels)} wheels + {len(locks)} pylock fixtures to {args.out}")


if __name__ == "__main__":
    main()
