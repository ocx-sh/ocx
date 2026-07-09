"""OCI registry client for acceptance tests.

Wraps oras-py to provide authenticated manifest inspection against any
OCI-compliant registry — local registry:2, Artifactory, GHCR, ECR, etc.
"""

from __future__ import annotations

import hashlib
import json
import urllib.error
import urllib.parse
import urllib.request
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


def fetch_platform_manifest_digest(
    registry: str, repo: str, tag: str, *, platform: str | None = None
) -> str:
    """Fetch the digest of a tag's platform MANIFEST (never the image index).

    Dependency metadata must pin platform manifest digests: a tag's image
    index is rewritten on every platform push and its old digest is
    garbage-collected (adr_dependency_manifest_pinning.md), so an index digest
    is not a durable pin. This helper resolves the tag and returns:

    - a flat image manifest's own digest, or
    - the single child's digest for a single-entry index, or
    - the child matching ``platform`` (``"os/arch"`` or ``"any"``) for a
      multi-entry index.
    """
    manifest = fetch_manifest_from_registry(registry, repo, tag)
    media_type = manifest.get("mediaType", "")
    if "image.manifest" in media_type:
        return fetch_manifest_digest(registry, repo, tag)

    entries = manifest.get("manifests", [])
    if platform is None:
        assert len(entries) == 1, (
            f"{registry}/{repo}:{tag} index has {len(entries)} children; "
            "pass platform= to select one"
        )
        return entries[0]["digest"]

    for entry in entries:
        plat = entry.get("platform") or {}
        key = f"{plat.get('os')}/{plat.get('architecture')}"
        if key == platform or (platform == "any" and plat.get("os") in (None, "any")):
            return entry["digest"]
    raise RuntimeError(f"no child for platform {platform} in {registry}/{repo}:{tag}")


def index_platforms(manifest: dict[str, Any]) -> set[str]:
    """Extract the set of 'os/architecture' strings from an OCI image index."""
    platforms: set[str] = set()
    for entry in manifest.get("manifests", []):
        plat = entry.get("platform")
        if plat:
            platforms.add(f"{plat['os']}/{plat['architecture']}")
    return platforms


# ---------------------------------------------------------------------------
# OCI 1.1 Referrers API helpers (raw HTTP, stdlib only)
#
# Used by the referrers smoke test (test_referrers_smoke.py, #195) to push a
# subject + a referrer and list it back via GET /v2/<name>/referrers/<digest>.
# Deliberately dependency-free (no oras-py) so the smoke test exercises the
# registry's referrers endpoint directly rather than a client abstraction.
# ---------------------------------------------------------------------------

IMAGE_MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"
IMAGE_INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"
_EMPTY_CONFIG = b"{}"
_EMPTY_CONFIG_MEDIA_TYPE = "application/vnd.oci.empty.v1+json"


def _sha256_digest(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def _http(
    method: str,
    url: str,
    *,
    data: bytes | None = None,
    headers: dict[str, str] | None = None,
) -> tuple[int, bytes, dict[str, str]]:
    """Issue an HTTP request, returning (status, body, headers) even on 4xx/5xx."""
    req = urllib.request.Request(url, data=data, method=method, headers=headers or {})
    try:
        resp = urllib.request.urlopen(req, timeout=10)
        return resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(), dict(exc.headers)


def push_blob(registry: str, repo: str, data: bytes, *, insecure: bool = True) -> str:
    """Upload a blob (two-step monolithic upload) and return its digest."""
    scheme = "http" if insecure else "https"
    digest = _sha256_digest(data)
    status, _, hdrs = _http("POST", f"{scheme}://{registry}/v2/{repo}/blobs/uploads/")
    if status not in (201, 202):
        raise RuntimeError(f"blob upload init failed ({status}) for {repo}")
    location = hdrs["Location"]
    if location.startswith("/"):
        location = f"{scheme}://{registry}{location}"
    sep = "&" if "?" in location else "?"
    status, body, _ = _http(
        "PUT",
        f"{location}{sep}digest={digest}",
        data=data,
        headers={"Content-Type": "application/octet-stream"},
    )
    if status != 201:
        raise RuntimeError(f"blob PUT failed ({status}): {body!r}")
    return digest


def push_manifest(
    registry: str,
    repo: str,
    manifest: dict[str, Any],
    *,
    reference: str | None = None,
    insecure: bool = True,
) -> tuple[str, dict[str, str]]:
    """PUT a manifest (by digest unless ``reference`` given); return (digest, headers)."""
    scheme = "http" if insecure else "https"
    body = json.dumps(manifest).encode()
    digest = _sha256_digest(body)
    ref = reference or digest
    status, resp, hdrs = _http(
        "PUT",
        f"{scheme}://{registry}/v2/{repo}/manifests/{ref}",
        data=body,
        headers={"Content-Type": manifest["mediaType"]},
    )
    if status != 201:
        raise RuntimeError(f"manifest PUT failed ({status}): {resp!r}")
    return digest, hdrs


def push_minimal_image(
    registry: str, repo: str, *, payload: bytes = b"subject", insecure: bool = True
) -> tuple[str, int]:
    """Push a minimal OCI image manifest (empty config + one layer).

    Returns ``(digest, manifest_byte_size)`` — the size is the exact number of
    bytes the manifest occupies, needed to build a subject descriptor.
    """
    config_digest = push_blob(registry, repo, _EMPTY_CONFIG, insecure=insecure)
    layer_digest = push_blob(registry, repo, payload, insecure=insecure)
    manifest = {
        "schemaVersion": 2,
        "mediaType": IMAGE_MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": _EMPTY_CONFIG_MEDIA_TYPE,
            "digest": config_digest,
            "size": len(_EMPTY_CONFIG),
        },
        "layers": [
            {
                "mediaType": "application/octet-stream",
                "digest": layer_digest,
                "size": len(payload),
            }
        ],
    }
    digest, _ = push_manifest(registry, repo, manifest, insecure=insecure)
    return digest, len(json.dumps(manifest).encode())


def push_referrer(
    registry: str,
    repo: str,
    subject_digest: str,
    subject_size: int,
    *,
    artifact_type: str,
    payload: bytes = b"referrer",
    insecure: bool = True,
) -> tuple[str, dict[str, str]]:
    """Push an OCI referrer (image manifest with a ``subject``); return (digest, headers).

    The returned headers carry ``OCI-Subject`` when the registry processed the
    subject — its presence proves native Referrers API support.
    """
    config_digest = push_blob(registry, repo, _EMPTY_CONFIG, insecure=insecure)
    layer_digest = push_blob(registry, repo, payload, insecure=insecure)
    manifest = {
        "schemaVersion": 2,
        "mediaType": IMAGE_MANIFEST_MEDIA_TYPE,
        "artifactType": artifact_type,
        "config": {
            "mediaType": _EMPTY_CONFIG_MEDIA_TYPE,
            "digest": config_digest,
            "size": len(_EMPTY_CONFIG),
        },
        "layers": [
            {
                "mediaType": "application/octet-stream",
                "digest": layer_digest,
                "size": len(payload),
            }
        ],
        "subject": {
            "mediaType": IMAGE_MANIFEST_MEDIA_TYPE,
            "digest": subject_digest,
            "size": subject_size,
        },
    }
    return push_manifest(registry, repo, manifest, insecure=insecure)


def list_referrers(
    registry: str,
    repo: str,
    subject_digest: str,
    *,
    artifact_type: str | None = None,
    insecure: bool = True,
) -> tuple[int, dict[str, Any] | None]:
    """GET /v2/<repo>/referrers/<subject_digest>.

    Returns ``(status, index)`` where ``index`` is the parsed OCI image index on
    a 200, else ``None``. A registry without the Referrers API returns a
    non-200 status (distribution v2 returns 404), so callers can distinguish
    "supported" from "unsupported" on ``status``.
    """
    scheme = "http" if insecure else "https"
    url = f"{scheme}://{registry}/v2/{repo}/referrers/{subject_digest}"
    if artifact_type:
        url += f"?artifactType={urllib.parse.quote(artifact_type, safe='')}"
    status, body, _ = _http("GET", url, headers={"Accept": IMAGE_INDEX_MEDIA_TYPE})
    if status != 200:
        return status, None
    return status, json.loads(body)


def index_platforms_with_features(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    """Extract the list of platform dicts (including os.features) from an OCI image index.

    Returns a list of dicts each containing at minimum the keys ``os``,
    ``architecture``, and ``os.features`` (the last may be absent or ``None``
    if the entry carries no feature tags).  This helper is used by libc-variant
    cascade tests to assert that both glibc and musl entries are preserved even
    though they share the same (os, arch) tuple.

    Example return value::

        [
            {"os": "linux", "architecture": "amd64", "os.features": ["libc.glibc"]},
            {"os": "linux", "architecture": "amd64", "os.features": ["libc.musl"]},
        ]
    """
    result: list[dict[str, Any]] = []
    for entry in manifest.get("manifests", []):
        plat = entry.get("platform")
        if plat:
            result.append({
                "os": plat.get("os"),
                "architecture": plat.get("architecture"),
                "os.features": plat.get("os.features"),
            })
    return result


def push_raw_package(
    registry: str,
    repo: str,
    tag: str,
    metadata: dict[str, Any],
    layer_tar_xz: bytes,
    *,
    platform: tuple[str, str],
) -> str:
    """Pushes an ocx PACKAGE directly via the registry HTTP API.

    Mirrors the `ocx package push` wire shape — image index with a platform
    entry -> image manifest (config blob = metadata JSON) -> tar+xz layer —
    WITHOUT invoking ocx, so tests can publish legacy shapes the push gate
    now rejects (e.g. index-pinned dependencies) and assert the read path
    stays backward compatible.

    Returns the pushed image index digest.
    """
    import json

    import requests

    layer_digest = _push_blob(registry, repo, layer_tar_xz)
    config_bytes = json.dumps(metadata).encode()
    config_digest = _push_blob(registry, repo, config_bytes)

    manifest = {
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "artifactType": "application/vnd.sh.ocx.package.v1",
        "config": {
            "mediaType": "application/vnd.sh.ocx.package.v1+json",
            "digest": config_digest,
            "size": len(config_bytes),
        },
        "layers": [
            {
                "mediaType": "application/vnd.oci.image.layer.v1.tar+xz",
                "digest": layer_digest,
                "size": len(layer_tar_xz),
            }
        ],
    }
    manifest_body = json.dumps(manifest).encode()
    manifest_digest = _sha256_digest(manifest_body)
    requests.put(
        f"http://{registry}/v2/{repo}/manifests/{manifest_digest}",
        data=manifest_body,
        headers={"Content-Type": "application/vnd.oci.image.manifest.v1+json"},
        timeout=10,
    ).raise_for_status()

    index = {
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "artifactType": "application/vnd.sh.ocx.package.v1",
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


def fetch_blob(registry: str, repo: str, digest: str) -> bytes:
    """Fetch a raw blob (e.g. a package config blob) from the registry."""
    import urllib.request

    url = f"http://{registry}/v2/{repo}/blobs/{digest}"
    with urllib.request.urlopen(url, timeout=10) as resp:
        return resp.read()


# ---------------------------------------------------------------------------
# Managed-config package push (raw registry HTTP, no oras dependency)
# ---------------------------------------------------------------------------

TAR_GZ_LAYER_MEDIA_TYPE = "application/vnd.oci.image.layer.v1.tar+gzip"


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
