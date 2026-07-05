"""OCI registry client for acceptance tests.

Wraps oras-py to provide authenticated manifest inspection against any
OCI-compliant registry — local registry:2, Artifactory, GHCR, ECR, etc.
"""

from __future__ import annotations

from typing import Any

import oras.client


def make_client(registry: str, *, insecure: bool = True) -> oras.client.OrasClient:
    """Create an OCI registry client for the given hostname.

    Parameters
    ----------
    registry:
        Registry hostname (e.g. ``localhost:5000``, ``ghcr.io``).
    insecure:
        Use HTTP instead of HTTPS.  Defaults to ``True`` for local
        test registries.
    """
    return oras.client.OrasClient(hostname=registry, insecure=insecure)


def fetch_manifest_from_registry(registry: str, repo: str, tag: str) -> dict[str, Any]:
    """Fetch and parse a raw OCI manifest from the registry."""
    client = make_client(registry)
    return client.get_manifest(f"{registry}/{repo}:{tag}")


def fetch_manifest_digest(registry: str, repo: str, tag: str) -> str:
    """Fetch the digest of an OCI manifest from the registry.

    Uses the Docker-Content-Digest header from a HEAD request, falling back
    to computing the digest from the manifest body.
    """
    import hashlib
    import json
    import urllib.request

    url = f"http://{registry}/v2/{repo}/manifests/{tag}"
    # Try with OCI manifest media types
    for media_type in [
        "application/vnd.oci.image.index.v1+json",
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.v2+json",
        "application/vnd.docker.distribution.manifest.list.v2+json",
    ]:
        req = urllib.request.Request(url, headers={"Accept": media_type})
        try:
            resp = urllib.request.urlopen(req, timeout=5)
            digest_header = resp.headers.get("Docker-Content-Digest")
            if digest_header:
                return digest_header
            # Fallback: compute from body
            body = resp.read()
            sha = hashlib.sha256(body).hexdigest()
            return f"sha256:{sha}"
        except urllib.error.HTTPError:
            continue

    raise RuntimeError(f"Could not fetch manifest digest for {registry}/{repo}:{tag}")


def index_platforms(manifest: dict[str, Any]) -> set[str]:
    """Extract the set of 'os/architecture' strings from an OCI image index."""
    platforms: set[str] = set()
    for entry in manifest.get("manifests", []):
        plat = entry.get("platform")
        if plat:
            platforms.add(f"{plat['os']}/{plat['architecture']}")
    return platforms


# ---------------------------------------------------------------------------
# Managed-config package push (raw registry HTTP, no oras dependency)
# ---------------------------------------------------------------------------

TAR_GZ_LAYER_MEDIA_TYPE = "application/vnd.oci.image.layer.v1.tar+gzip"


def _sha256_digest(data: bytes) -> str:
    import hashlib

    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def _push_blob(registry: str, repo: str, data: bytes) -> str:
    """Pushes `data` as a blob to `repo`, returning its digest.

    Uses the plain two-step OCI distribution monolithic-upload flow (POST to
    start, PUT with `?digest=` to complete) so this helper has no dependency
    on the `oras` client for a custom, non-image artifact shape.
    """
    import requests

    digest = _sha256_digest(data)
    head = requests.head(f"http://{registry}/v2/{repo}/blobs/{digest}", timeout=10)
    if head.status_code == 200:
        return digest

    start = requests.post(f"http://{registry}/v2/{repo}/blobs/uploads/", timeout=10)
    start.raise_for_status()
    location = start.headers["Location"]
    upload_url = location if location.startswith("http") else f"http://{registry}{location}"
    separator = "&" if "?" in upload_url else "?"
    put = requests.put(
        f"{upload_url}{separator}digest={digest}",
        data=data,
        headers={"Content-Type": "application/octet-stream"},
        timeout=10,
    )
    put.raise_for_status()
    return digest


def make_config_layer(
    config_toml: bytes,
    *,
    entry_name: str = "config.toml",
    extra_entries: dict[str, bytes] | None = None,
) -> bytes:
    """Builds a gzip'd tar layer carrying ``entry_name`` (+ optional extras).

    ``entry_name`` is a knob for malformed-shape negatives (e.g. a package
    without ``config.toml``).
    """
    import gzip
    import io
    import tarfile

    tar_buffer = io.BytesIO()
    with tarfile.open(fileobj=tar_buffer, mode="w") as tar:
        entries: dict[str, bytes] = {entry_name: config_toml}
        entries.update(extra_entries or {})
        for name, data in entries.items():
            info = tarfile.TarInfo(name=name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    # mtime=0 keeps the layer deterministic across runs (recordings).
    return gzip.compress(tar_buffer.getvalue(), mtime=0)


def push_raw_config_package(
    registry: str,
    repo: str,
    tag: str,
    config_toml: bytes,
    *,
    platform: tuple[str, str] = ("any", "any"),
    entry_name: str = "config.toml",
    extra_entries: dict[str, bytes] | None = None,
    layer_media_type: str = TAR_GZ_LAYER_MEDIA_TYPE,
) -> str:
    """Pushes a managed-config PACKAGE directly via the registry HTTP API.

    Mirrors the v2 wire shape `ocx config push` produces — image index with a
    platform entry -> image manifest -> tar+gzip layer containing
    ``config.toml`` — without invoking ocx, so tests can also construct
    malformed-shape negatives via the keyword knobs (``platform``,
    ``entry_name``, ``extra_entries``, ``layer_media_type``). For the product
    path use ``src.helpers.push_managed_config`` instead.

    Returns the pushed image index's digest (``sha256:<hex>``) — the tier's
    drift identity.
    """
    import json

    import requests

    layer_bytes = make_config_layer(config_toml, entry_name=entry_name, extra_entries=extra_entries)
    layer_digest = _push_blob(registry, repo, layer_bytes)
    config_bytes = b"{}"
    config_digest = _push_blob(registry, repo, config_bytes)

    manifest = {
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": len(config_bytes),
        },
        "layers": [
            {
                "mediaType": layer_media_type,
                "digest": layer_digest,
                "size": len(layer_bytes),
            }
        ],
    }
    manifest_body = json.dumps(manifest).encode()
    manifest_digest = _sha256_digest(manifest_body)
    response = requests.put(
        f"http://{registry}/v2/{repo}/manifests/{manifest_digest}",
        data=manifest_body,
        headers={"Content-Type": "application/vnd.oci.image.manifest.v1+json"},
        timeout=10,
    )
    response.raise_for_status()

    index = {
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [
            {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": manifest_digest,
                "size": len(manifest_body),
                "platform": {"os": platform[0], "architecture": platform[1]},
            }
        ],
    }
    index_body = json.dumps(index).encode()
    response = requests.put(
        f"http://{registry}/v2/{repo}/manifests/{tag}",
        data=index_body,
        headers={"Content-Type": "application/vnd.oci.image.index.v1+json"},
        timeout=10,
    )
    response.raise_for_status()
    return response.headers.get("Docker-Content-Digest") or _sha256_digest(index_body)
