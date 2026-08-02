# Architecture Discovery: Referrers (`ocx verify` / `ocx sbom`)

Phase 1 of `/swarm-plan max 24`. Factual current-state map, no design opinions.

## 1. OCI Client Surface

**`Client` struct** — `crates/ocx_lib/src/oci/client.rs:85`

```rust
pub struct Client {
    transport: Box<dyn OciTransport>,
    pub(super) lock_timeout: std::time::Duration,
    pub(super) tag_chunk_size: usize,
    pub(super) repository_chunk_size: usize,
}
```

**Public methods on `Client`** (line citations in `crates/ocx_lib/src/oci/client.rs`):

| Method | Line | Purpose |
|--------|------|---------|
| `ensure_auth(identifier, operation)` | 126 | Pre-auth before operations |
| `list_tags(identifier)` | 136 | Paginated tag listing |
| `list_repositories(registry)` | 145 | Registry catalog |
| `fetch_manifest_digest(identifier)` | 156 | HEAD manifest (digest only) |
| `fetch_manifest(identifier)` | 165 | Pull manifest + digest |
| `pull_manifest(pinned_id)` | 277 | Pull+verify image manifest |
| `pull_metadata(pinned_id, manifest)` | 304 | Pull config blob as `Metadata` |
| `pull_layer(pinned_id, layer, metadata, dir)` | 339 | Download+extract single layer |
| `push_package(info, layers)` | 438 | Push package manifest+index |
| `push_description(identifier, desc)` | 673 | Push `__ocx.desc` artifact |
| `pull_description(identifier, temp_dir)` | 764 | Pull description artifact |

**`OciTransport` trait** — `crates/ocx_lib/src/oci/client/transport.rs:33`

Methods: `ensure_auth`, `list_tags`, `catalog`, `fetch_manifest_digest`, `pull_manifest_raw`, `pull_blob`, `pull_blob_to_file`, `head_blob`, `push_manifest`, `push_manifest_raw`, `push_blob`, `box_clone`.

**No `list_referrers` or `pull_referrer` method exists in `OciTransport`.** Trait has no referrer-related surface at all.

**`NativeTransport`** — `crates/ocx_lib/src/oci/client/native_transport.rs:35`

Wraps `oci::native::Client` (= `oci_client::client::Client`). Implements `OciTransport`. Has direct access to the upstream `pull_referrers` call but does not expose it.

**`pull_referrers` in patched `oci-client`** — `external/rust-oci-client/src/client.rs:1659`

```rust
pub async fn pull_referrers(
    &self,
    image: &Reference,
    artifact_type: Option<&str>,
) -> Result<OciImageIndex>
```

Calls `GET /v2/{repository}/referrers/{digest}?artifactType={filter}` (OCI Distribution Spec referrers API). Requires a digest-bearing `Reference` (tag-only refs return an error at line 1855). Returns an `OciImageIndex` whose `manifests` entries each describe one referrer. Handles the standard Referrers API. Does NOT implement the fallback tag schema (`sha256-{digest}`) — comment at `external/rust-oci-client/src/manifest.rs:104–106` explicitly states that responsibility falls on the consumer.

**`subject` field confirmation** — `external/rust-oci-client/src/manifest.rs:108`

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub subject: Option<OciDescriptor>,
```

Present in `OciImageManifest`. Serializes/deserializes with `serde`. Currently always `None` in OCX because `push_multi_layer_manifest` uses `..Default::default()` (`crates/ocx_lib/src/oci/client.rs:650`) → `subject: None`. The field is wired for deserialization — incoming manifests from registries that include `subject` will have it populated.

**Error types relevant to referrers** — `crates/ocx_lib/src/oci/client/error.rs`

| Variant | Line | Meaning for referrers |
|---------|------|----------------------|
| `ManifestNotFound(String)` | 29 | 404 from referrers endpoint |
| `Registry(Box<dyn Error>)` | 41 | HTTP errors (405 or otherwise) |
| `Serialization(serde_json::Error)` | 51 | Malformed referrer index |
| `Authentication(...)` | 15 | 401 from referrers endpoint |

`ClassifyExitCode` on `ClientError`: `ManifestNotFound` → `ExitCode::NotFound`, `Registry` → `ExitCode::Unavailable`, `Authentication` → `ExitCode::AuthError`.

A new `ReferrersUnsupported` variant (for registries without the Referrers API, notably GHCR) would map to `ExitCode::Unavailable` or a new code — architect's call.

**Auth/retry/caching** — `ensure_auth` called before every transport operation. Token cache lives inside `oci_client::Client`. No retry logic in OCX; relies on upstream HTTP retries. No caching of referrer responses (in-memory or disk) exists today.

## 2. CLI Command Wiring

**`Command` enum** — `crates/ocx_cli/src/command.rs:41`

New subcommands slot into `Command` as new variants:

```rust
pub enum Command {
    // existing...
    Verify(verify::Verify),   // ← new
    Sbom(sbom::Sbom),         // ← new
}
```

Add `pub mod verify;` and `pub mod sbom;` to `command.rs`, add arms to `Command::execute()` (line 78). One file per subcommand in `crates/ocx_cli/src/command/`.

**Command execute signature** — `crates/ocx_cli/src/command.rs:78`

```rust
pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode>
```

Commands return `anyhow::Result<ExitCode>` where `ExitCode` is `std::process::ExitCode`. The typed `ocx_lib::cli::ExitCode` is returned via `.into()` at the `main.rs` success path.

**Typed exit codes** — `crates/ocx_lib/src/cli/exit_code.rs:20`

Variants: `Success=0`, `Failure=1`, `UsageError=64`, `DataError=65`, `Unavailable=69`, `IoError=74`, `TempFail=75`, `PermissionDenied=77`, `ConfigError=78`, `NotFound=79`, `AuthError=80`, `OfflineBlocked=81`.

`classify_error` free function — `crates/ocx_lib/src/cli/classify.rs:58` — walks `std::error::Error::source()` chain, downcasts to known error types. New error types need `ClassifyExitCode` impl + `try_downcast!` entry in `classify.rs:76`.

**Output format** — `crates/ocx_cli/src/options/format.rs:7`

```rust
pub enum Format { Json, Plain }  // default: Plain
```

Global `--format json` flag via `ContextOptions` (`crates/ocx_cli/src/app/context_options.rs:29`). `Api::report(&item)` dispatches to `item.print_json()` or `item.print_plain()` via the `Printable` trait (`crates/ocx_cli/src/api.rs:45`). No per-command `--format` flag.

**Report data types** — new files would go in `crates/ocx_cli/src/api/data/verify.rs` and `api/data/sbom.rs` implementing `Printable + serde::Serialize`.

**`Context` struct** — `crates/ocx_cli/src/app/context.rs:19`

Provides `remote_client() → Result<&oci::Client>` (line 126). Returns `Err(ocx_lib::Error::OfflineMode)` when offline. New verify/sbom commands call `context.remote_client()?` directly (same pattern as `package_info.rs:35`).

## 3. Package Manager Context

**`PackageManager` struct** — `crates/ocx_lib/src/package_manager.rs:25`

Fields: `file_structure`, `index`, `client: Option<oci::Client>`, `default_registry`, `profile`.

**Natural integration point for auto-verify hook**: `PackageManager::pull` — `crates/ocx_lib/src/package_manager/tasks/pull.rs:71`. This is where a package download completes and the fully-resolved `PinnedIdentifier` is available before symlinks are created. An opt-in post-pull hook would call `client.list_referrers(pinned_id.digest(), Some(SIGNATURE_ARTIFACT_TYPE))` here.

`PackageManager::install` (`crates/ocx_lib/src/package_manager/tasks/install.rs:26`) delegates to `pull` then creates symlinks — `pull` is the narrower anchor because it runs once per unique digest even for shared dependencies.

## 4. Index / Cache Layout

**`~/.ocx/` top-level directories** (from `crates/ocx_lib/src/file_structure.rs:60`):

```
~/.ocx/
├── blobs/{registry_slug}/{algorithm}/{2hex}/{30hex}/
│   ├── data           (raw OCI blob bytes — manifests, image indexes, referrer indexes)
│   └── digest         (full digest string for recovery)
├── layers/{registry_slug}/{algorithm}/{2hex}/{30hex}/
│   ├── content/       (extracted tar layer)
│   └── digest
├── packages/{registry_slug}/{algorithm}/{2hex}/{30hex}/
│   ├── content/       (assembled from layers/ via hardlinks)
│   ├── metadata.json
│   ├── manifest.json
│   ├── resolve.json
│   ├── install.json
│   ├── digest
│   └── refs/{symlinks,deps,layers,blobs}/
├── tags/{registry_slug}/{repo_path}.json    (tag→digest maps)
├── symlinks/{registry_slug}/{repo}/
│   ├── candidates/{tag}  →  packages/.../content/
│   └── current           →  packages/.../content/
└── temp/{32hex}/          (download staging)
```

**Where referrer metadata might cache**: No dedicated store today. Raw referrer index blobs (OCI `ImageIndex` JSON) could go into `blobs/` — content-addressed by digest, fitting the existing CAS model. `BlobStore` at `crates/ocx_lib/src/file_structure/blob_store.rs` is the natural landing spot. Separate `referrers/` store not needed; existing `blobs/` tier handles raw OCI blobs of all kinds.

## 5. Reusable Primitives

| Need | Existing Asset | Location |
|------|----------------|----------|
| Identifier parsing with registry default | `options::Identifier::transform_all()` | `crates/ocx_cli/src/options/identifier.rs` |
| Digest struct (SHA-256/384/512) | `oci::Digest`, `oci::Algorithm` | `crates/ocx_lib/src/oci/digest.rs` |
| Pinned identifier (digest-guaranteed) | `oci::PinnedIdentifier` | `crates/ocx_lib/src/oci/pinned_identifier.rs` |
| OCI transport auth + retry | `NativeTransport` wrapping `oci_client::Client` | `crates/ocx_lib/src/oci/client/native_transport.rs` |
| OCI blob pull to memory | `OciTransport::pull_blob` | `crates/ocx_lib/src/oci/client/transport.rs:75` |
| OCI manifest pull (raw bytes + digest) | `OciTransport::pull_manifest_raw` | `crates/ocx_lib/src/oci/client/transport.rs:65` |
| Raw referrers API call | `oci_client::Client::pull_referrers` | `external/rust-oci-client/src/client.rs:1659` |
| `ClientError` types and `ClassifyExitCode` | `ClientError` enum | `crates/ocx_lib/src/oci/client/error.rs` |
| JSON read/write with path context | `SerdeExt::read_json` / `write_json` | `crates/ocx_lib/src/utility/` (prelude) |
| Filesystem path slugification | `StringExt::to_relaxed_slug` | prelude |
| Blob store (CAS raw blobs) | `BlobStore` | `crates/ocx_lib/src/file_structure/blob_store.rs` |
| Progress spinner for async ops | `crate::cli::progress::spinner_span` | `crates/ocx_lib/src/cli/progress.rs` |
| `DirWalker` for local blob scanning | `utility::fs::DirWalker` | `crates/ocx_lib/src/utility/fs/dir_walker.rs` |
| Mock transport for unit tests | `StubTransport` / `StubTransportData` | `crates/ocx_lib/src/oci/client/test_transport.rs` |
| `Printable` trait for output | `Printable` | `crates/ocx_cli/src/api.rs:19` |

**What does NOT exist and must be built**:
- `OciTransport::list_referrers(image, artifact_type)` method
- `Client::list_referrers(identifier, artifact_type)` public method
- `Client::pull_referrer_manifest(descriptor)` or reuse `pull_manifest_raw`
- Report data types `VerifyReport`, `SbomReport` implementing `Printable`
- `command/verify.rs`, `command/sbom.rs` command handlers
- Optional `ClientError::ReferrersUnsupported` variant (for graceful GHCR degradation)

## 6. Extension Points / Seams

**Seam 1: `OciTransport` trait** — `crates/ocx_lib/src/oci/client/transport.rs:33`

Add a new async method to the trait:

```rust
async fn list_referrers(
    &self,
    image: &oci::native::Reference,
    artifact_type: Option<&str>,
) -> Result<oci::ImageIndex>;
```

All three implementors (`NativeTransport`, `StubTransport`, future test doubles) must gain this method. `NativeTransport` delegates to `self.client.pull_referrers(image, artifact_type)`.

**Seam 2: `Client` public facade** — `crates/ocx_lib/src/oci/client.rs` (~line 867)

```rust
pub async fn list_referrers(
    &self,
    identifier: &oci::PinnedIdentifier,
    artifact_type: Option<&str>,
) -> std::result::Result<oci::ImageIndex, ClientError>
```

Takes `PinnedIdentifier` (not `Identifier`) because referrers API requires a digest — type system enforces this.

**Seam 3: `Command` enum** — `crates/ocx_cli/src/command.rs:41`

Add `Verify(verify::Verify)` and `Sbom(sbom::Sbom)` variants. Add `pub mod verify;` and `pub mod sbom;` declarations. Add match arms in `execute()`.

**Seam 4: `crates/ocx_cli/src/command/verify.rs`** (new file)

Pattern: call `context.remote_client()?`, build `oci::PinnedIdentifier` from CLI arg (resolve tag → digest via `context.default_index()`), call `client.list_referrers(pinned, Some(COSIGN_ARTIFACT_TYPE))`, build `VerifyReport`, call `context.api().report(&report)?`.

**Seam 5: `crates/ocx_lib/src/cli/classify.rs:76` (`try_classify` function)**

Add `try_downcast!` entries for new error types. Add new `ClassifyExitCode` impls next to each new error type definition.

## Module Map

| Module | Key Types | Relevance |
|--------|-----------|-----------|
| `crates/ocx_lib/src/oci/client.rs` | `Client` | Add `list_referrers` public method |
| `crates/ocx_lib/src/oci/client/transport.rs` | `OciTransport` trait | Add `list_referrers` to trait |
| `crates/ocx_lib/src/oci/client/native_transport.rs` | `NativeTransport` | Implement via patched client |
| `crates/ocx_lib/src/oci/client/test_transport.rs` | `StubTransport` | Add stub impl for unit tests |
| `crates/ocx_lib/src/oci/client/error.rs` | `ClientError` | Possibly add `ReferrersUnsupported` |
| `crates/ocx_lib/src/cli/exit_code.rs` | `ExitCode` | No change needed |
| `crates/ocx_lib/src/cli/classify.rs` | `classify_error`, `try_classify` | Add downcast entries |
| `external/rust-oci-client/src/client.rs` | `pull_referrers` (line 1659) | Upstream impl exists |
| `external/rust-oci-client/src/manifest.rs` | `OciImageManifest.subject` (line 108) | Field present, not set on push |
| `crates/ocx_cli/src/command.rs` | `Command` enum | Add Verify/Sbom variants |
| `crates/ocx_cli/src/app/context.rs` | `Context` | `remote_client()` at line 126 |
| `crates/ocx_cli/src/api.rs` | `Api`, `Printable` | No change; new data types use it |
| `crates/ocx_lib/src/package_manager/tasks/pull.rs` | `PackageManager::pull` (line 71) | Future auto-verify hook anchor |
| `crates/ocx_lib/src/file_structure.rs` | `FileStructure`, `BlobStore` | Referrer blobs cache in existing `blobs/` tier |

## Cross-Module Flow for `ocx verify <ref>`

```
CLI: command/verify.rs
  → options::Identifier::transform_all(ref, default_registry)   [identifier parsing]
  → context.default_index().select(identifier, platforms)        [resolve tag → digest]
  → context.remote_client()?                                     [Err on --offline]
  → client.list_referrers(&pinned_id, Some(SIG_ARTIFACT_TYPE))
      → transport.ensure_auth(image, Pull)
      → transport.list_referrers(image, artifact_type)           [NEW in OciTransport]
          → NativeTransport: self.client.pull_referrers(image, artifact_type)
          → GET /v2/{repo}/referrers/{digest}?artifactType=...
          → Returns OciImageIndex (list of referrer descriptors)
  → for each referrer descriptor: pull blob (signature/SBOM data)
      → transport.pull_blob(image, &descriptor_digest)
  → Build VerifyReport from referrer descriptors
  → context.api().report(&report)?
```
