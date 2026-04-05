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
