#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Publish a local, stamped ocx build to a local registry, so a second checkout can pin it.

    scripts/local_publish.py registry [--stop]
    scripts/local_publish.py publish [--push ghcr]

`registry` runs the toolchain's `zot` on localhost:$OCX_LOCAL_REGISTRY_PORT (default 15000), its
store under $XDG_STATE_HOME/ocx/dev-registry/<port>, with on-demand sync from ghcr.io: a package the local
registry lacks is pulled from ghcr.io on first request, so the registry stands in for ghcr.io
through `OCX_MIRRORS`. Running it twice is a no-op that prints the URL.

`publish` cross-builds the host triple stamped as the dev channel, scans it for test provenance,
packages it unsigned and pushes `<cargo version>-dev_<UTC>` (cascading) to
localhost:<port>/ocx-sh/dev/cli. `--push ghcr` also pushes it to ghcr.io/ocx-sh/dev/cli with the
token in GITHUB_TOKEN or GH_TOKEN, and refuses when that package does not exist yet.

A lock made against a local build records a digest only this machine's registry holds: it is local-only.

Exit codes: 0 done, 1 refused or a step failed (one line on stderr says why).
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import platform
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import UTC, datetime
from pathlib import Path

import tomllib

DEFAULT_PORT = 15000
LOCAL_PATH = "ocx-sh/dev/cli"
GHCR = "ghcr.io"
# (system, machine) -> (Bazel triple, OCX platform)
HOSTS = {
    ("Linux", "x86_64"): ("x86_64-unknown-linux-gnu", "linux/amd64+libc.glibc"),
    ("Linux", "aarch64"): ("aarch64-unknown-linux-gnu", "linux/arm64+libc.glibc"),
    ("Darwin", "x86_64"): ("x86_64-apple-darwin", "darwin/amd64"),
    ("Darwin", "arm64"): ("aarch64-apple-darwin", "darwin/arm64"),
    ("Windows", "AMD64"): ("x86_64-pc-windows-gnullvm", "windows/amd64"),
    ("Windows", "ARM64"): ("aarch64-pc-windows-gnullvm", "windows/arm64"),
}
_DIRECT = urllib.request.build_opener(urllib.request.ProxyHandler({}))


class Refused(Exception):
    """A step the user must fix first; the message is the one-line reason."""


def port() -> int:
    raw = os.environ.get("OCX_LOCAL_REGISTRY_PORT", str(DEFAULT_PORT))
    if not (raw.isascii() and raw.isdigit() and 1 <= int(raw) <= 65535):
        raise Refused(f"OCX_LOCAL_REGISTRY_PORT must be a port in 1-65535, got '{raw}'")
    return int(raw)


def state_dir(listen: int) -> Path:
    # Keyed by port: a second registry's start must not overwrite the pid of a running one.
    base = os.environ.get("XDG_STATE_HOME") or str(Path.home() / ".local" / "state")
    return Path(base) / "ocx" / "dev-registry" / str(listen)


def zot_config(store: Path, listen: int) -> dict[str, object]:
    return {
        "distSpecVersion": "1.1.1",
        "storage": {"rootDirectory": str(store)},
        "http": {"address": "127.0.0.1", "port": str(listen)},
        "log": {"level": "info"},
        "extensions": {
            "sync": {
                "enable": True,
                "registries": [
                    {"urls": [f"https://{GHCR}"], "onDemand": True, "tlsVerify": True}
                ],
            }
        },
    }


def registry_up(listen: int) -> bool:
    try:
        with _DIRECT.open(f"http://127.0.0.1:{listen}/v2/", timeout=2) as response:
            return response.status == 200
    except (urllib.error.URLError, ConnectionError, TimeoutError):
        return False


def start_registry(listen: int, state: Path) -> str:
    """Start zot unless it already answers; return the URL."""
    url = f"http://localhost:{listen}"
    if registry_up(listen):
        return url
    if shutil.which("zot") is None:
        raise Refused(
            "zot is not on PATH; pull the project toolchain with `ocx pull` (or run via `ocx exec -- …`)"
        )
    state.mkdir(parents=True, exist_ok=True)
    config = state / "config.json"
    config.write_text(
        json.dumps(zot_config(state / "store", listen), indent=2) + "\n",
        encoding="utf-8",
    )
    with (state / "zot.log").open("ab") as log:
        proc = subprocess.Popen(
            ["zot", "serve", str(config)],
            stdout=log,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
        )
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise Refused(f"zot exited with {proc.returncode}; see {state / 'zot.log'}")
        if registry_up(listen):
            # Only a confirmed start records its pid, so a failed one never replaces a live registry's.
            (state / "zot.pid").write_text(f"{proc.pid}\n", encoding="utf-8")
            return url
        time.sleep(0.2)
    proc.terminate()
    raise Refused(f"zot did not answer on {url} within 30s; see {state / 'zot.log'}")


def stop_registry(state: Path) -> None:
    pidfile = state / "zot.pid"
    if pidfile.is_file():
        try:
            os.kill(int(pidfile.read_text(encoding="utf-8")), signal.SIGTERM)
        except ProcessLookupError:
            pass
        pidfile.unlink()


def cargo_version(root: Path) -> str:
    with (root / "Cargo.toml").open("rb") as handle:
        return tomllib.load(handle)["workspace"]["package"]["version"]


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Refuse every 3xx: the default handler replays `Authorization` at the redirect target, even across origins."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: ANN001 - urllib's signature
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


def ghcr_package_exists(token: str) -> bool:
    """True when the ghcr.io dev package answers a tags listing for this token."""
    request = urllib.request.Request(
        f"https://{GHCR}/v2/{LOCAL_PATH}/tags/list",
        headers={
            "Authorization": "Bearer " + base64.b64encode(token.encode()).decode()
        },
    )
    try:
        with _OPENER.open(request, timeout=20):
            return True
    except urllib.error.HTTPError as err:
        if err.code in (401, 403, 404):
            return False
        raise Refused(f"{GHCR} answered HTTP {err.code} to the package check") from None
    except (urllib.error.URLError, OSError) as err:
        raise Refused(f"cannot reach {GHCR}: {type(err).__name__}") from None


def run(cmd: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> None:
    result = subprocess.run(cmd, cwd=cwd, env=env, check=False)
    if result.returncode != 0:
        raise Refused(f"{cmd[0]} failed with exit {result.returncode}")


def publish(root: Path, push: str | None, now: datetime | None = None) -> list[str]:
    """Build, scan and publish; return the lines to print."""
    listen = port()
    if not registry_up(listen):
        raise Refused(
            f"no registry on localhost:{listen}; run `task release:local:registry` first"
        )
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN") or ""
    if push == "ghcr":
        if not token:
            raise Refused("PUSH=ghcr needs GITHUB_TOKEN or GH_TOKEN in the environment")
        if not ghcr_package_exists(token):
            raise Refused(
                f"ghcr.io/{LOCAL_PATH} does not exist (or the token cannot see it); create it before PUSH=ghcr"
            )
    elif push is not None:
        raise Refused(f"unknown PUSH target '{push}' (only 'ghcr')")

    host = HOSTS.get((platform.system(), platform.machine()))
    if host is None:
        raise Refused(f"no build target for {platform.system()} {platform.machine()}")
    triple, ocx_platform = host
    stamp = (now or datetime.now(UTC)).strftime("%Y%m%d%H%M%S")
    base = cargo_version(root)
    tag = f"{base}-dev_{stamp}"
    exe = (
        root
        / "target"
        / "cross-release"
        / triple
        / ("ocx.exe" if "windows" in triple else "ocx")
    )

    run(
        [
            "task",
            "cross:build:release",
            f"TARGET={triple}",
            "CHANNEL=dev",
            f"VERSION={base}-dev+{stamp}",
        ],
        cwd=root,
    )
    run(
        [
            sys.executable,
            "scripts/release_provenance_check.py",
            "--scan",
            str(exe),
            "--min-files",
            "1",
        ],
        cwd=root,
    )

    # The binary under release does the packaging, and the local registry is plain HTTP.
    env = {
        **os.environ,
        "PATH": f"{exe.parent}{os.pathsep}{os.environ['PATH']}",
        "OCX_INSECURE_REGISTRIES": f"localhost:{listen}",
    }
    publish_cmd = [
        "bash",
        "scripts/publish_ocx_package.sh",
        "--platform",
        ocx_platform,
        "--binary",
        str(exe),
    ]
    run(
        [
            *publish_cmd,
            "--identifier",
            f"localhost:{listen}/{LOCAL_PATH}:{tag}",
            "--no-keep-tag",
            "--cascade",
        ],
        cwd=root,
        env=env,
    )
    if push == "ghcr":
        ghcr_env = {
            **env,
            f"OCX_AUTH_{GHCR.replace('.', '_')}_TYPE": "basic",
            f"OCX_AUTH_{GHCR.replace('.', '_')}_USER": "ocx",
            f"OCX_AUTH_{GHCR.replace('.', '_')}_TOKEN": token,
        }
        run(
            [
                *publish_cmd,
                "--identifier",
                f"{GHCR}/{LOCAL_PATH}:{tag}",
                "--no-keep-tag",
            ],
            cwd=root,
            env=ghcr_env,
        )

    return [
        f"tag: {tag}",
        f"export OCX_MIRRORS='{json.dumps({GHCR: f'http://localhost:{listen}'}, separators=(',', ':'))}'",
        f"export OCX_INSECURE_REGISTRIES='localhost:{listen}'",
        f'ocx.toml pin in a second checkout: ocx = "{GHCR}/{LOCAL_PATH}:{tag}"',
        "A lock made against this build is local-only: its digest exists only in this machine's registry.",
        "check the build in use with: ocx exec -- ocx version --verbose",
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = parser.add_subparsers(dest="cmd", required=True)
    reg = sub.add_parser("registry", help="start the local registry (idempotent)")
    reg.add_argument(
        "--stop", action="store_true", help="stop the registry this script started"
    )
    pub = sub.add_parser("publish", help="build and publish the host triple")
    pub.add_argument("--push", default=None, help="also push to ghcr (value: ghcr)")
    args = parser.parse_args(argv)
    root = Path(__file__).resolve().parents[1]
    try:
        if args.cmd == "registry":
            if args.stop:
                stop_registry(state_dir(port()))
                return 0
            listen = port()
            print(start_registry(listen, state_dir(listen)))
        else:
            print("\n".join(publish(root, args.push or None)))
    except Refused as err:
        print(f"local_publish: {err}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
