"""Baseline floor command builder — curl+tar.

Constructs the plain-download-and-extract command used as the theoretical
floor for bench scenarios:

    curl -sS <blob-url-via-proxy> | tar -xJ -C <tmpdir>

This represents the minimum overhead of HTTP download + xz extraction with no
OCX overhead (no OCI manifest fetch, no metadata parsing, no symlink creation).
hyperfine compares this command directly against `ocx install` so the ratio
appears in the JSON export.

The blob URL is constructed from the PackageInfo returned by make_package():
  http://<proxy_host>/v2/<repo>/blobs/<digest>

The digest is the OCI layer blob digest, obtained by querying the registry
manifest for the package's layer descriptor.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import NamedTuple


class BaselineCommand(NamedTuple):
    """The curl+tar command and a temp-dir prepare command for a floor scenario.

    Attributes
    ----------
    download_url:
        Full HTTP URL to the compressed layer blob (via toxiproxy endpoint).
    prepare_cmd:
        Shell command to run before each hyperfine run (wipes tmpdir, recreates it).
    bench_cmd:
        The hyperfine command string: `curl ... | tar -xJ -C $tmpdir`.
        Uses a shell variable `$tmpdir` that is set by prepare_cmd.
    """

    download_url: str
    prepare_cmd: str
    bench_cmd: str


def _fetch_manifest(registry: str, repo: str, tag: str) -> dict:
    """Fetch the OCI manifest for repo:tag from the registry.

    Returns the parsed manifest JSON dict (image manifest or image index).
    Accepts manifests for both OCI and Docker media types.
    """
    url = f"http://{registry}/v2/{repo}/manifests/{tag}"
    req = urllib.request.Request(
        url,
        headers={
            "Accept": (
                "application/vnd.oci.image.manifest.v1+json,"
                "application/vnd.docker.distribution.manifest.v2+json,"
                "application/vnd.oci.image.index.v1+json,"
                "application/vnd.docker.distribution.manifest.list.v2+json"
            )
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read())
    except urllib.error.URLError as e:
        msg = f"Failed to fetch manifest for {registry}/{repo}:{tag}: {e}"
        raise RuntimeError(msg) from e


def _resolve_platform_manifest(registry: str, repo: str, manifest: dict) -> dict:
    """If manifest is an image index, resolve to the first platform manifest.

    For bench purposes we only need one platform's layer blob, so we pick
    the first entry in the index (deterministic across runs on the same host).
    """
    media_type = manifest.get("mediaType", "")
    if "index" in media_type or "list" in media_type:
        manifests = manifest.get("manifests", [])
        if not manifests:
            msg = "Image index has no platform manifests"
            raise RuntimeError(msg)
        first = manifests[0]
        digest = first["digest"]
        url = f"http://{registry}/v2/{repo}/manifests/{digest}"
        req = urllib.request.Request(
            url,
            headers={
                "Accept": (
                    "application/vnd.oci.image.manifest.v1+json,"
                    "application/vnd.docker.distribution.manifest.v2+json"
                )
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                return json.loads(resp.read())
        except urllib.error.URLError as e:
            msg = f"Failed to fetch platform manifest {digest}: {e}"
            raise RuntimeError(msg) from e
    return manifest


def build_baseline_command(
    registry: str,
    proxy_host: str,
    repo: str,
    tag: str,
    *,
    scratch_dir: str | None = None,
) -> BaselineCommand:
    """Build the curl+tar floor command for a package.

    Parameters
    ----------
    registry:
        The registry address for manifest lookups (e.g. "localhost:5000").
        Manifests are fetched directly from the registry (not via proxy)
        because toxiproxy is only used during the timed benchmark run itself.
    proxy_host:
        The toxiproxy endpoint through which the blob download is routed
        during the benchmark (e.g. "localhost:5002"). This ensures the
        floor measurement is subject to the same network conditions as the
        ocx install benchmark.
    repo:
        OCI repository name (e.g. "bench-pkg-10mb").
    tag:
        OCI tag (e.g. "1.0.0").
    scratch_dir:
        Root directory for the curl extract temp dir.  Must be disk-backed
        (NOT /tmp which is tmpfs/RAM on WSL2).  Defaults to
        ``<bench_scratch_dir>/curl-extract`` when None — callers should pass
        ``str(BENCH_SCRATCH_DIR / "curl-extract")`` from harness.py.
        The fallback default is ``/tmp/bench-baseline-tmp`` for backward
        compatibility with ``__main__`` standalone usage only.

    Returns
    -------
    BaselineCommand
        Named tuple with prepare_cmd and bench_cmd ready for hyperfine.
    """
    manifest = _fetch_manifest(registry, repo, tag)
    manifest = _resolve_platform_manifest(registry, repo, manifest)

    layers = manifest.get("layers", [])
    if not layers:
        msg = f"Manifest for {repo}:{tag} has no layers"
        raise RuntimeError(msg)

    # Use the first (and typically only) layer blob for the floor benchmark.
    layer = layers[0]
    digest = layer["digest"]  # e.g. "sha256:abc123..."
    download_url = f"http://{proxy_host}/v2/{repo}/blobs/{digest}"

    # prepare_cmd: wipe and recreate a temp directory.
    # The bench_cmd uses $tmpdir as a shell variable populated by prepare_cmd.
    # IMPORTANT: use the caller-supplied scratch_dir (disk-backed) — never /tmp
    # (tmpfs/RAM-backed on WSL2 and many Linux systems) to avoid OOM.
    extract_dir = scratch_dir if scratch_dir is not None else "/tmp/bench-baseline-tmp"
    prepare_cmd = f"rm -rf {extract_dir} && mkdir -p {extract_dir}"
    bench_cmd = f"curl -sS {download_url} | tar -xJ -C {extract_dir}"

    return BaselineCommand(
        download_url=download_url,
        prepare_cmd=prepare_cmd,
        bench_cmd=bench_cmd,
    )


if __name__ == "__main__":
    import sys

    if len(sys.argv) < 4:  # noqa: PLR2004
        print(
            "Usage: python -m bench.baseline <registry> <proxy_host> <repo> <tag>",
            file=sys.stderr,
        )
        sys.exit(1)
    registry_arg, proxy_arg, repo_arg, tag_arg = (
        sys.argv[1],
        sys.argv[2],
        sys.argv[3],
        sys.argv[4],
    )
    cmd = build_baseline_command(registry_arg, proxy_arg, repo_arg, tag_arg)
    print(f"Blob URL:    {cmd.download_url}")
    print(f"Prepare:     {cmd.prepare_cmd}")
    print(f"Bench cmd:   {cmd.bench_cmd}")
