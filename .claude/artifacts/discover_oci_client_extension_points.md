# OCI Client Extension Points: Phase 1 Discovery

Scope: `crates/ocx_lib/src/oci/**` and `external/rust-oci-client/**`. Feeds design of `list_referrers` support.

## 1. Transport Layer

### OciTransport Trait

**Location:** `crates/ocx_lib/src/oci/client/transport.rs:33`

Async contract for wire-level OCI operations. All implementations handle auth via `ensure_auth()` called before operations.

**Method signatures** (line citations):
- `ensure_auth(image, operation)` (ln 41)
- `list_tags(...) → Result<Vec<String>>` (ln 46–51)
- `catalog(...) → Result<Vec<String>>` (ln 54–59)
- `fetch_manifest_digest(image) → Result<String>` (ln 62)
- `pull_manifest_raw(image, accepted_media_types) → Result<(Vec<u8>, String)>` (ln 65–69)
- `pull_blob(image, digest) → Result<Vec<u8>>` (ln 75)
- `pull_blob_to_file(image, digest, path, total_size, on_progress) → Result<()>` (ln 83–90)
- `head_blob(image, digest) → Result<u64>` (ln 95)
- `push_manifest(image, manifest) → Result<String>` (ln 100)
- `push_manifest_raw(image, data, media_type) → Result<String>` (ln 104–109)
- `push_blob(image, data, digest, on_progress) → Result<String>` (ln 116–122)

### NativeTransport Implementation

**Location:** `crates/ocx_lib/src/oci/client/native_transport.rs:34–242`

Wraps `oci_client::Client` (patched fork at `external/rust-oci-client`). Auth: explicit `auth_for(image)` for some methods, internal for others. Push auth pre-cached via `authenticate()` (ln 54–61).

**Error mapping** (ln 64–95):
- `registry_error(e)` wraps any OCI error as `ClientError::Registry` (ln 64–66)
- `manifest_not_found_or_registry_error(e, image)` → `ClientError::ManifestNotFound` on 404/MANIFEST_UNKNOWN/NameUnknown (ln 71–95)
  - Handles `ImageManifestNotFoundError` variant
  - Inspects `OciErrorCode` enum for `ManifestUnknown`, `NotFound`, `NameUnknown`
  - Falls back on `ServerError { code: 404 }`

**Key helper**: `blob_not_found()` (error.rs:77–92) constructs `ClientError::BlobNotFound` from image reference + blob digest with debug assertions.

## 2. Existing Endpoint Methods (Inventory)

### Authentication
- `ensure_auth(identifier, operation)` → `transport.ensure_auth()` (client.rs:126–130)

### Read — List
- `list_tags(identifier)` (client.rs:136–143) — paginated via `paginate()` (ln 875–895)
- `list_repositories(registry)` (client.rs:145–153) — same pattern

### Read — Fetch
- `fetch_manifest_digest(identifier)` (client.rs:156–162) — digest-only
- `fetch_manifest(identifier)` (client.rs:165–171) — calls `fetch_manifest_raw()` helper (client.rs:854–865), deserializes to `oci::Manifest`
- `pull_manifest(pinned_identifier)` (client.rs:277–299) — validates digest, extracts `ImageManifest`; rejects `ImageIndex` → `UnexpectedManifestType`
- `pull_metadata(pinned_identifier, manifest?)` (client.rs:304–329) — config blob → `Metadata`
- `pull_layer(pinned_identifier, layer_descriptor, metadata, output_dir)` (client.rs:339–387) — blob → file w/ progress, verifies digest via `verify_blob_digest()` (ln 55–83)

### Helper
- `fetch_manifest_raw(image)` private (client.rs:854–865) — accepts `ACCEPTED_MANIFEST_MEDIA_TYPES`

### Write — Manifest
- `merge_platform_into_index(...)` (client.rs:183–260) — fetch/create ImageIndex, merge platform entry, push
- `push_manifest_and_merge_tags(...)` (client.rs:457–501) — orchestration
- `push_multi_layer_manifest(...)` (client.rs:512–599+) — concurrent layer upload (LAYER_PUSH_CONCURRENCY = 4)

## 3. Natural Slot for `list_referrers()`

**Best analogue:** `list_tags()` (client.rs:136–143)

**Return type pattern:** Mirror `fetch_manifest()` → `(Digest, oci::Manifest)` for each referrer entry, or just return the raw `oci::ImageIndex` (what the upstream crate returns).

**Proposed method signature:**

```rust
pub async fn list_referrers(
    &self,
    identifier: &PinnedIdentifier,
    artifact_type: Option<&str>,
) -> Result<oci::ImageIndex, ClientError>
```

**Placement:** After `fetch_manifest()` (~line 172), before `merge_platform_into_index()`.

**Implementation flow:**
1. Call `self.transport.ensure_auth(image, Pull)`
2. Call `self.transport.list_referrers(image, artifact_type)` (new transport method)
3. Return `Result<oci::ImageIndex, ClientError>`

**Transport addition** (transport.rs):

```rust
async fn list_referrers(
    &self,
    image: &oci::native::Reference,
    artifact_type: Option<&str>,
) -> Result<oci::ImageIndex>;
```

Native impl (native_transport.rs): as shipped, `self.client.pull_referrers_native(image, artifact_type)` → map errors via `registry_error()`, with `Ok(None)` becoming `ClientError::ReferrersUnsupported`. The OCI referrers tag-schema fallback is owned by OCX (`OciTransport::list_referrers_with_fallback`), not by the fork.

## 4. Error Taxonomy

| HTTP status | Current mapping | Semantics for referrers |
|-------------|-----------------|--------------------------|
| **404 Not Found** | `ClientError::ManifestNotFound` | Registry does NOT support Referrers API, OR no referrers for this digest. **Ambiguous in current taxonomy.** |
| **405 Method Not Allowed** | `ClientError::Registry` | Referrers endpoint unsupported (some older registries) |
| **200 with empty manifests[]** | `Ok(empty index)` | Supported, no referrers — not an error |
| **401 Unauthorized** | `ClientError::Authentication` | Standard auth failure |
| **5xx** | `ClientError::Registry` | Transient registry issue |

**Current idiom for "registry capability missing"**: no dedicated variant. `ClientError::ManifestNotFound` conflates "artifact 404" and "endpoint 404".

**Proposed (architect decides):**
- **Option A — reuse**: keep using `ManifestNotFound`; document the overload in docstring. User-facing message still useful.
- **Option B — new variant**: add `ClientError::ReferrersUnsupported(String)` for cleaner CLI messaging ("this registry does not support OCI referrers — signatures and SBOMs unavailable"). Maps to `ExitCode::Unavailable`.

The ADR `adr_oci_artifact_enrichment.md:377` mandates "OCX detects support at runtime — if the Referrers API returns 404/405, a clear message is surfaced." Option B aligns with that contract; Option A requires care to produce that message without ambiguity.

## 5. Caching Hooks

**Current state:** `Client` has no built-in blob/manifest caching.
- `pull_manifest_raw()` calls transport every time
- `RemoteIndex` has in-memory cache (shared via `Arc<RwLock>`)
- `LocalIndex` has disk JSON + in-memory cache (tags + manifest chains)

**Referrers caching strategy (v1):** 
- No caching in `Client` layer; cache at `Index` layer only if referrers become frequent
- Referrer lists are small (<100 entries typical), transient discovery data
- The content-addressed `BlobStore` already handles OCI blobs; caching a referrer index's raw JSON there is an existing pattern

**Note:** subsystem-oci.md mentions a cache coherence issue ("execute commands call context.remote_client() directly"). Referrers should route through `Index` layer to avoid this — architect should consider.

## 6. Patched oci-client Crate

**Location:** `external/rust-oci-client/` (git submodule)
**Version:** 0.16.1 (Cargo.toml:22)

### OciImageManifest.subject Field

**ALREADY PRESENT** (manifest.rs:101–108):

```rust
/// This is an optional subject linking this manifest to another manifest
/// forming an association between the image manifest and the other manifest.
#[serde(skip_serializing_if = "Option::is_none")]
pub subject: Option<OciDescriptor>,
```

Wired in `Default` impl (ln 138).

### Referrers API Support

**ALREADY IMPLEMENTED UPSTREAM** — `external/rust-oci-client/src/client.rs:1659`:

```rust
pub async fn pull_referrers(
    &self,
    image: &Reference,
    artifact_type: Option<&str>,
) -> Result<OciImageIndex>
```

- Requires digest-bearing `Reference` (tag-only → error at ln 1855)
- Returns `OciImageIndex`
- Does NOT implement the fallback tag schema (`sha256-{digest}`) — consumer responsibility per comment at manifest.rs:104–106
- Handles `GET /v2/{repository}/referrers/{digest}?artifactType={filter}`

**Net: no upstream patch is needed.** OCX only needs to add a new `OciTransport::list_referrers` method that delegates to this existing upstream function.

## Summary

**Transport layer:** `OciTransport` trait is the clear extension point. Add `list_referrers(image, artifact_type)`.

**Client layer:** Add public `list_referrers(identifier, artifact_type)` after `fetch_manifest()`, delegate through transport.

**Error handling:** Architect chooses between reusing `ManifestNotFound` (simpler) and adding `ReferrersUnsupported` variant (cleaner UX). Option B recommended given ADR contract.

**Caching:** No changes in `Client`; content-addressed `BlobStore` already fits referrer indexes if local caching is added later.

**Patched crate:** `OciImageManifest.subject` present; `Client::pull_referrers` also present. **No upstream PR required.**
