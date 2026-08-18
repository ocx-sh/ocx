# Test Fixture Patterns: Phase 1 Discovery

Scope: `test/**`. Documents current acceptance-test infrastructure for later `ocx verify` / `ocx sbom` tests against a registry pre-populated with OCI v1.1 referrers.

## 1. Registry Fixture

**Location:** `test/docker-compose.yml`

```yaml
services:
  registry:
    image: registry:2
    ports:
      - "5000:5000"
```

- **Image:** `registry:2` (unversioned — latest stable, ≥2.8.1 as of 2026-04).
- **Port:** `localhost:5000`.
- **Referrers API:** supported as of registry:2 v2.7+ (`adr_oci_artifact_enrichment.md:375`). Unversioned tag resolves ≥2.7 → API is guaranteed.

**Startup hook** (`test/conftest.py`):

```python
def pytest_sessionstart(session: pytest.Session) -> None:
    if os.environ.get("PYTEST_XDIST_WORKER") is not None:
        return
    registry = os.environ.get("REGISTRY", "localhost:5000")
    start_registry(registry)

@pytest.fixture(scope="session")
def registry() -> str:
    addr = os.environ.get("REGISTRY", "localhost:5000")
    start_registry(addr)
    return addr
```

Helper: `test/src/helpers.py::start_registry()` (lines 37–54) — GET `/v2/` probe, docker-compose up, 0.5s poll, 15s timeout.

## 2. Existing Fixtures

### Package Publishing

**`make_package(ocx, repo, tag, tmp_path, *, new=False, cascade=False, outputs=...)` → PackageInfo** — `test/src/helpers.py:87–214`

- Creates, bundles, pushes test package
- Returns `PackageInfo(fq, short, marker, platform)`
- `new=True` adds `-n` flag for new repo
- `cascade=True` adds `--cascade` for multi-tag push

### Manifest Fetching

**`_fetch_manifest(registry, repo, ref)` → dict** — `test/tests/test_multi_layer.py:265–270`

```python
def _fetch_manifest(registry: str, repo: str, ref: str) -> dict:
    url = f"http://{registry}/v2/{repo}/manifests/{ref}"
    accept = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json"
    req = urllib.request.Request(url, headers={"Accept": accept})
    resp = urllib.request.urlopen(req, timeout=5)
    return json.loads(resp.read())
```

Demonstrates HTTP manifest inspection pattern already in use. Foundation for a future `_fetch_referrers(registry, repo, digest, artifact_type?)`.

### ORAS Client

**`test/pyproject.toml:11`** — `oras>=0.2.42` in dev deps.

**`test/src/registry.py:14–25`:**

```python
import oras.client

def make_client(registry: str, *, insecure: bool = True) -> oras.client.OrasClient:
    return oras.client.OrasClient(hostname=registry, insecure=insecure)
```

## 3. Options for Attaching a Referrer from a Test

### Option A: `oras` CLI (High Alignment — RECOMMENDED)

**Pros:**
- Already in `pyproject.toml` (oras ≥0.2.42 — Python SDK; the CLI is separately available via OCX's `.ocx/index/` mirror — dogfood pattern)
- Industry-standard (cosign, Syft, ORAS community use it)
- Minimal setup: one CLI command per referrer
- Matches existing test style (subprocess shell-out via `OcxRunner`)

**Cons:**
- Requires `oras` binary in PATH at test time — solved by adding to `.ocx/index/` and `ocx install oras`

**Pattern:**

```python
subprocess.run([
    "oras", "attach",
    "--artifact-type", "application/vnd.sh.ocx.signature.v1",
    f"localhost:5000/{repo}@sha256:{subject_digest}",
    "signature.payload",
], check=True, env={"ORAS_ALLOW_HTTP": "true"})
```

### Option B: `oras-py` SDK (Moderate Alignment)

**Pros:** pure Python, already imported via `test/src/registry.py`.
**Cons:** SDK less mature than CLI; abstraction layers obscure test intent.

### Option C: HTTP PUT (Manual Manifest Crafting)

**Pros:** transparent; enables testing edge cases (malformed manifest, auth failures).
**Cons:** error-prone digest computation; requires config-blob push first; significant code volume.

Matches the existing `_fetch_manifest` style (urllib) but inverted.

**Pattern:**

```python
manifest = {
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "subject": {
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "digest": f"sha256:{subject_digest}",
        "size": subject_size,
    },
    "artifactType": "application/vnd.sh.ocx.signature.v1",
    "config": { "mediaType": "application/vnd.oci.empty.v1+json", "digest": EMPTY_DIGEST, "size": 2 },
    "layers": [{ "mediaType": "...", "digest": "sha256:...", "size": N }],
}
url = f"http://localhost:5000/v2/{repo}/manifests/{tag}"
urllib.request.urlopen(urllib.request.Request(
    url,
    data=json.dumps(manifest).encode(),
    headers={"Content-Type": "application/vnd.oci.image.manifest.v1+json"},
    method="PUT",
))
```

**Ranking:**

1. **Option A (oras CLI)** — low setup, high familiarity, dogfoods OCX; matches existing test patterns
2. **Option C (HTTP PUT)** — keeps transparency for crafted edge cases (malformed `subject`, unusual `artifactType`); useful for a small number of paranoid tests
3. **Option B (oras-py SDK)** — fallback if CLI not available

**Recommendation for the design:** use **Option A** for the happy-path fixture (`published_package_with_signature_referrer`, `published_package_with_sbom`), and **Option C** for edge cases that need malformed manifests. This matches OCX's existing style: shell out for standard ops, craft HTTP for edge cases.

## 4. Existing SBOM / Signature Tests

**NONE EXIST.**

- Searched `test/tests/` for `referrer|sbom|signature|cosign|oras` — only hit is a comment in `test_multi_layer.py:136` ("referrer-only" manifests, conceptual only).
- No tests exercise signature verification, SBOM discovery, or Referrers API endpoints.

**Conclusion:** Green field. All referrer-attachment and verification tests are new.

## 5. Python Helpers for OCI Manifest Crafting

### Existing

**`test/src/registry.py`:**

- `make_client(registry, insecure=True) → oras.client.OrasClient`
- `fetch_manifest_from_registry(registry, repo, tag) → dict`
- `fetch_manifest_digest(registry, repo, tag) → str`
- `index_platforms(manifest) → set[str]`

**`test/tests/test_multi_layer.py`:**

- `_make_layer_content(tmp_path, name, files) → Path`
- `_bundle_layer(ocx, layer_dir, tmp_path, ext) → Path`
- `_push_multi_layer(ocx, repo, tag, layers, tmp_path, **kwargs)`
- `_fetch_manifest(registry, repo, ref) → dict`
- `_fetch_layer_digest(registry, repo, tag) → str`

### Missing — Referrer-Specific

No existing helper for:
- Creating a manifest with `subject` field
- Pushing a referrer artifact
- Listing referrers via the Referrers API endpoint

**Proposed additions to `test/src/registry.py`:**

```python
def push_referrer(
    registry: str,
    repo: str,
    subject_digest: str,
    artifact_type: str,
    layers: list[tuple[str, bytes, str]],  # (name, content, media_type)
) -> str:
    """Push a referrer artifact with subject binding. Returns referrer manifest digest."""
    ...

def list_referrers(
    registry: str,
    repo: str,
    subject_digest: str,
    artifact_type_filter: str | None = None,
) -> list[dict]:
    """GET /v2/{repo}/referrers/{digest}?artifactType=... → descriptor list."""
    ...
```

## 6. Test Commands & Markers

### Execution

```bash
task test              # full suite (build + registry + tests)
task test:quick        # skip rebuild
task test:parallel     # pytest-xdist (-n auto)
cd test && uv run pytest tests/test_file.py::test_name -v --no-build
```

### Fixtures Available

- `ocx: OcxRunner` — ocx binary subprocess wrapper
- `unique_repo: str` — UUID-prefixed repo for isolation
- `tmp_path: Path` — per-test temp dir
- `registry: str` — session-scoped `localhost:5000`

### Proposed New Fixture

```python
@pytest.fixture()
def published_package_with_referrer(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> tuple[PackageInfo, str]:
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    # Push signature referrer via oras CLI (Option A)
    referrer_digest = push_referrer(...)
    return pkg, referrer_digest
```

## Summary Table

| Aspect | Finding | Citation |
|--------|---------|----------|
| Registry image | `registry:2` (unversioned) | `test/docker-compose.yml:3` |
| Referrers API support | Yes, v2.7+ | `adr_oci_artifact_enrichment.md:375` |
| Registry port | `localhost:5000` | `test/conftest.py:22` |
| Package push helper | `make_package(...)` | `test/src/helpers.py:87–214` |
| Manifest fetch utility | `_fetch_manifest(...)` | `test/tests/test_multi_layer.py:265–270` |
| ORAS SDK | `oras>=0.2.42` | `test/pyproject.toml:11` |
| ORAS wrapper | `make_client(...)` | `test/src/registry.py:14–25` |
| Existing referrer tests | None | grep of `test/tests/` → 0 hits |
| Recommended approach | oras CLI (Option A) + HTTP PUT (Option C) for edge cases | — |
