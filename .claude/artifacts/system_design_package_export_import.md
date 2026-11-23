# System design: package export and import over blob directories

**Status:** Draft, companion to `.claude/artifacts/adr_package_export_import.md` (Proposed, 2026-11-23, review round 1 and re-validation applied)
**Date:** 2026-11-23
**Scope:** the export directory and its marker, blob directories, the blob cache, `ocx package export`, `ocx package import`, root `ocx export`, root `ocx import`, and the #759 work package. Trade-offs live in the ADR; this file holds contracts precise enough to write failing tests from.

## 1. Inputs and fixed points

| Fixed point | Where | Why it constrains the design |
|---|---|---|
| Home blob layout `blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/{data,digest}` | `crates/ocx_store/src/file_structure/blob_store.rs:23-67` | The export reuses it verbatim |
| `BlobStore::read_blob` reads whole files, no cap, no re-hash | `blob_store.rs:123-131` | The lookup must not route layers through it |
| GC classifies any `data` in a valid CAS dir; ignores extra subdirectories | `blob_store.rs:159-168` | `referrers/` markers inside a blob dir are safe |
| `IndexFormatConfig` field order is wire | `crates/ocx_index/src/wire.rs:30-39` | The export marker is a separate file |
| `IndexRoot.repository` is a required string | `wire.rs:54-56` | A new root needs a placeholder pointer |
| `PackageRef` keeps registry case; only the repository refuses uppercase | `crates/ocx_oci/src/package_ref.rs:296-321` | Slug collisions are folded and refused |
| Layer seam: home `layers/` checked before transport and client | `crates/ocx_package_manager/src/tasks/pull.rs:924-947` | Lookup goes between the home check and `require_client` |
| Storage keyed by logical `pinned.registry()` | `pull.rs:913`, `:1024-1025` | Lookup keys by logical registry too |
| `record_origin` runs whenever metadata was not provided | `pull.rs:448-449`, `:521-528` | Needs a network gate |
| `verified_sources` trusts `refs/origins/`; lowercases the registry | `crates/ocx_project/src/consent.rs:186-229` | A lookup hit must mint no origin |
| Layer pipeline polled on a blocking thread (`spawn_blocking` + `SyncIoBridge`) | `crates/ocx_oci/src/client.rs:958-1006` | The tee may use `std::fs` |
| Layer checks: cap, then completeness (`ShortBlobRead` 75), then digest (65) | `client.rs:1007-1047`, `client/error.rs:37` | Local short reads are remapped to 65 |
| Copy re-hashes every streamed blob | `crates/ocx_oci/src/copy.rs:382-407` | Copy's source read may come from the lookup |
| Copy resolves an index-served source tag through `Index::resolve_version` | `crates/ocx_package/src/publisher/copy.rs:364-386`, `crates/ocx_index/src/lib.rs:346-368` | Copy by tag needs one rule change, for derived names |
| Copy caps referrers per leaf at 256; sidecar tags HEADed at the source | `copy.rs:27`, `copy.rs:236-306` | Shared marker cap; sidecar skip rule |
| Verify caps candidates at 8 and filters `artifactType` client-side | `crates/ocx_sign/src/verify/pipeline.rs:55`, `:418-440` | Marker filter runs before the cap |
| Auto-verify's client is built even under `--offline` | `crates/ocx_cli/src/app/context.rs:35-38`, `crates/ocx_package_manager/src/tasks/auto_verify.rs:45` | Import needs a lookup-only referrer source |
| `OcxIndexConfig` accepts an injected transport | `crates/ocx_index/src/ocx_index.rs:390-570` | Import builds a source over `FileIndexTransport` |
| `OcxIndex::source_kind` is always `Published`; `kind_for_registry` is private and falls back to `Derived` for a registry no source serves | `ocx_index.rs:1251`, `chained_index.rs:196-207` | Needs a public accessor; an unconfigured registry imports as `Derived` |
| An offline tag miss is `PolicyResolutionBlocked`, slug `index_resolution_blocked`, exit 81 | `chained_index.rs:313-324`, `ocx_index/src/error.rs:190-196`, `ocx_exit/src/exit_code.rs:73` | Exit for an offline export of an unknown tag |
| `INDEX_REFRESH_CONCURRENCY` is `pub(super)` in the CLI; a structural test names it | `crates/ocx_cli/src/command/index_common.rs:16`, `:209` | Moves to `ocx_index` |
| `OCX_CONFIG` is a `PathList`, split by `std::env::split_paths` | `crates/ocx_env/src/vars.rs:56-65` | `OCX_BLOB_DIR` copies it |
| Lock entries record leaf digests per platform only | `crates/ocx_project/src/lock.rs:133-150` | Root export writes no index |
| Ledger writers: pull render, lock, mutation; `register` always rewrites | `render_toolchain.rs:464`, `ocx_project/src/lock.rs:707`, `mutation.rs:179`, `registry.rs:89-176` | `exec` registers nowhere (#759); needs a no-write path |
| Per-prompt hook composes `LocalOnly` | `crates/ocx_package_manager/src/activation.rs:456` | The hook fetches nothing, so it does not register |
| Packages cannot declare `OCX_*` env keys (publish time) | `crates/ocx_package/src/metadata/validation.rs:186-193` | A package cannot set `OCX_BLOB_DIR` |
| Patch descriptor layer read from home blobs | `crates/ocx_package_manager/src/tasks/patch_discovery.rs:1129` | Named exception to "home keeps no layer archives" |
| `ocx_oci` may not depend on `ocx_store` | `scripts/crate_map.toml:28-33` | Lookup, markers and path live in `ocx_oci` |

## 2. C4 level 1 — system context

```
 [Operator / CI job] --ocx package export / ocx export--> [ocx (online machine)]
 [ocx (online machine)] --HTTPS pulls--> [OCI registry]
 [ocx (online machine)] --writes--> [Export directory DIR]
 [DIR] --carried by sneakernet / artifact store / cache action--> [ocx (target machine)]
 [Operator / CI job] --ocx package import DIR / ocx import DIR / --blob-dir DIR/blobs--> [ocx (target machine)]
 [ocx (any machine)] --reads/writes--> [Blob cache directory (shared by CI jobs)]
 [Later Bazel track] --reads blobs/<registry>/<alg>/...--> [DIR]
```

Trust boundaries: the registry (TLS, auth); `DIR` (untrusted bytes, digest-checked; its pins trusted as an index source); the cache (semi-trusted, digest-checked, self-healed, never a provenance source).

## 3. C4 level 2 — containers

| Container | Owns |
|---|---|
| `ocx_cli` | Argv, settings resolution for `--blob-dir` / `--blob-cache-dir`, the four commands, the two reports |
| `ocx_package_manager` | Export and import tasks, pull's layer seam and origin gate, auto-verify's referrer source |
| `ocx_index` | `ChainedIndex` lookup seam, `LocalIndex::import_tags`, the export index writer, `PACKAGE_REFRESH_CONCURRENCY`, the derived-name `resolve_version` rule |
| `ocx_oci` | `BlobLookup`, `BlobWriter`, `blob_data_path`, the marker grammar, the cache tee, `cas_path`, copy's source seam |
| `ocx_sign` | Verify's referrer seam (`ReferrerSource::{Live, LookupOnly}`) |
| `ocx_config` / `ocx_env` | `[blobs]` section, `OCX_BLOB_DIR`, `OCX_BLOB_CACHE_DIR` |
| `ocx_store` | Home `BlobStore` calls `blob_data_path` and the marker functions |
| `ocx_project` | `ProjectRegistry::register_if_absent` (#759) |

## 4. C4 level 3 — components

```
Context::try_init
  ├─ resolve BlobSettings (flag ▸ env ▸ config)            [ocx_cli app/blob_settings.rs]
  ├─ BlobLookup::new(dirs, cache)                          [ocx_oci blob_lookup]
  ├─ Index::from_chained_with_content_store(..).with_blob_lookup(lookup)   [ocx_index]
  └─ PackageManager::new(..).with_blob_lookup(lookup)      [ocx_package_manager]

pull: extract_layer_atomic
  home layers/ ─hit→ reuse
     └miss→ lookup.open_stream(logical registry, digest) ─clean→ Client::extract_layer_from_stream
            (bad copy: warn, remember, next source)       └miss→ require_client → Client::pull_layer (+ cache tee)
  origin recorded only if the leaf manifest came from the network
ChainedIndex::fetch_blob (and every content-store read)
  home blobs ─hit→ verify → return
     └miss→ lookup.read_metadata ─clean→ write home → return
                                 └miss→ sources (network) → write home + cache → return
verify (ocx_sign)
  Live:       transport listing first, then home and lookup markers
  LookupOnly: home and lookup markers only; subject and referrer blobs from home/lookup
copy (ocx_oci copy.rs)
  lookup (source registry as written) → transport
export task
  resolve tags through the ambient chain (commits first-sight pins to the home)
  blobs: lookup order → BlobWriter(DIR/blobs) under the per-DIR lock
  index: LocalIndex writers over IndexStore(DIR/index); catalog last; marker last
import task
  read marker → map dirs → tag plan → pull_all(tag@digest, read-only view) → commit pins per package
root import task
  marker check → project pull with DIR/blobs, offline, LookupOnly → register project
```

## 5. Code level — lookup chain and cache write

### 5.1 Lookup chain

```
fn open_stream(registry, digest, declared_size):
    for (index, dir) in dirs:                       # read-only, list order
        path = blob_data_path(dir, registry, digest)
        if let Some(file) = open_regular(path):     # O_NONBLOCK + handle is_file
            return Stream { origin: Dir(index), file }
    if cache:
        path = blob_data_path(cache.root, registry, digest)
        if let Some(file) = open_regular(path): return Stream { origin: Cache, file }
    return None                                     # caller goes to the network

caller loop (pull's layer seam, read_metadata):
    bad_dir = 0; discarded = 0
    for each source in lookup order, then network (if the walk has one):
        result = read and verify (digest, completeness)
        clean            -> debug!(origin); return
        bad, Dir(i)      -> warn; bad_dir += 1; continue
        bad, Cache       -> warn; cache.remove (warn on failure); discarded += 1; continue   # a miss
        network error    -> if bad_dir > 0: Err(DigestMismatch)        # exit 65
                            else Err(network error, "discarded {discarded} cache entries")
    end of sources (import has no network step; --offline stops before it):
        if bad_dir > 0:  Err(DigestMismatch)                           # exit 65, every case
        elif import:     Err(ExportNotFound { blob, package })         # exit 79
        else:            Err(OfflineMode, "discarded {discarded} cache entries")  # exit 81
    a short blob-directory read counts as bad, never ShortBlobRead; a short cache read is a miss
    each layer retry starts in a fresh output directory
```

### 5.2 Cache write (tee and metadata)

```
pull_layer_with_caps(.., cache: Option<(&BlobCacheDir, &str /*logical registry*/)>):
    raw    = transport.pull_blob_streaming(..)
    capped = raw.take(blob_total_size)
    tee    = cache.map(|(c, reg)| c.writer(reg, digest))   # temp in <cache>/.tmp/
    hashed = HashingAsyncReader::new(Tee(capped, tee), alg)
    ... decoder, tar, drain (unchanged; polled inside spawn_blocking via SyncIoBridge) ...
    if completeness ok and digest ok: tee.finish()   # sync_data, ancestor check, create_dir_all, persist
    else: drop(tee)                                  # temp deleted
```

The tee writes with `std::fs`, which is correct on the blocking thread the chain runs on (`client.rs:958-1006`). `finish` never fails the pull: a cache write failure is one warning.

## 6. Contracts

Stable IDs. A test cites the ID it proves.

### Components and types

**C-001 `ocx_oci::cas_path::cas_shard_path(digest: &Digest) -> PathBuf`.** Moved from `ocx_store`. Output unchanged: `<alg>/<hex[0..2]>/<hex[2..32]>`. The `ocx_store` copy is deleted.

**C-002 `ocx_oci::blob_lookup::BlobLookup`.**
```rust
pub fn blob_data_path(root: &Path, registry: &str, digest: &Digest) -> PathBuf;
pub const MAX_REFERRERS_PER_SUBJECT: usize = 256;

pub struct BlobLookup { dirs: Vec<PathBuf>, cache: Option<BlobCacheDir> }
impl BlobLookup {
    pub fn new(dirs: Vec<PathBuf>, cache: Option<BlobCacheDir>) -> Self;
    pub fn is_empty(&self) -> bool;
    pub fn with_leading_dir(&self, dir: PathBuf) -> Self;   // import: DIR/blobs first
    pub fn cache(&self) -> Option<&BlobCacheDir>;
    pub async fn open_stream(&self, registry: &str, digest: &Digest)
        -> Result<Option<LookupStream>, LookupError>;
    pub async fn read_metadata(&self, registry: &str, digest: &Digest, cap: u64)
        -> Result<Option<Vec<u8>>, LookupError>;
    pub async fn referrers(&self, registry: &str, subject: &Digest)
        -> Result<ReferrerMarkers, LookupError>;
}
pub struct LookupStream { pub origin: LookupOrigin, pub reader: tokio::fs::File, pub size: u64 }
pub enum LookupOrigin { Dir(usize), Cache }
pub struct ReferrerMarkers { pub digests: Vec<Digest>, pub complete: bool }
```
- `registry` is the logical registry as written. Every path comes from `blob_data_path`.
- `open_stream` opens with `O_NONBLOCK` on Unix and checks `is_file` on the handle. A non-regular file is absent. A symlink to a regular file is followed.
- `read_metadata` reads through `ocx_util::fs::read_bounded(path, cap)`. Over cap is `LookupError::OverCap` (65 `invalid_manifest`). It hashes before returning and applies § 5.1's bad-copy rule.
- `referrers` reads at most `MAX_REFERRERS_PER_SUBJECT` marker names per root, deduplicated, sorted. A name that is not a valid digest is skipped with a warning. `complete` is true when any root holds `referrers/.complete`.
- A missing dir from env or config warns once and is skipped (C-015 marks it).
- Never reads `.tmp/`. Never enumerates a registry directory. Never reads the marker file.

**C-003 `ocx_oci::blob_lookup::BlobCacheDir`.**
```rust
pub struct BlobCacheDir { root: PathBuf }
impl BlobCacheDir {
    pub async fn open(root: PathBuf) -> Result<Self, LookupError>;  // creates root and .tmp/ lazily
    pub fn writer(&self, registry: &str, digest: &Digest) -> Result<BlobWriter, LookupError>;
    pub async fn write_verified(&self, registry: &str, digest: &Digest, bytes: &[u8]) -> Result<(), LookupError>;
    pub async fn remove(&self, registry: &str, digest: &Digest) -> Result<(), LookupError>;
}
```
- `open` refuses a root with a symlinked ancestor it did not create (`refuse_if_symlink_in_path`), exit 74.
- `remove` checks ancestors, then removes the `data` file itself, never a link target.
- Writing a new registry slug updates `CACHE/ocx-export.json` best effort (C-017). A slug that collides with another registry warns and skips the write.

**C-004 `ocx_oci::blob_lookup::BlobWriter`** (streaming CAS writer, ADR Decision 8 protocol).
```rust
impl BlobWriter {
    pub fn root_for(root: &Path, registry: &str, digest: &Digest) -> Result<Self, LookupError>;
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()>;   // also hashes
    pub fn finish(self) -> Result<WriteOutcome, LookupError>;        // blocking
}
pub enum WriteOutcome { Written, Replaced, AlreadyPresent }
```
- Occupant check first: verified is `AlreadyPresent`; a `digest` file naming another full digest is `LookupError::ShardCollision`; otherwise the occupant is corrupt and is replaced (`Replaced`).
- Temp in `<root>/.tmp/` with the umask default mode (`tempfile::Builder` with permissions `0o666` before umask; spike S-WP2).
- `finish` compares the running hash with `digest`; mismatch deletes the temp and returns `DigestMismatch`.
- `sync_data`, ancestor check, `create_dir_all(parent)`, then `persist_temp_file_noclobber` (new) or `persist_temp_file` (replace). `Occupied` re-verifies the winner.
- Windows: rename retry up to 2 s on `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION`, `ERROR_FILE_NOT_FOUND`; success if the target appeared and verifies.
- Writes the sibling `digest` file through the same temp-and-rename.

**C-005 `ocx_oci::Client::extract_layer_from_stream(stream: LookupStream, layer: &Descriptor, output_dir: &Path)`.** The body of `pull_layer_with_caps` with the source stream injected. Same caps and pipeline. One difference: a short read maps to `DigestMismatch` (65), not `ShortBlobRead`. `pull_layer` calls the same inner function with the network stream and the optional cache tee.

**C-006 `Client::with_blob_cache(cache: BlobCacheDir) -> Client`.** The tee writes under the logical registry passed by the caller: `pull_layer(.., cache_key: Option<&str>)`. `None` disables the tee for that call.

**C-007 `ocx_index::Index::with_blob_lookup(self, lookup: BlobLookup) -> Self`**, **`Index::source_kind_for(&self, registry: &str) -> SourceKind`**, **`ocx_index::PACKAGE_REFRESH_CONCURRENCY: usize = 8`**.
- Every digest-addressed read that consults the attached content store calls `lookup.read_metadata` after a home miss and before any source. A hit is written to the home. A source fetch also calls `lookup.cache().write_verified`. Under `ChainMode::Offline` the lookup is still consulted.
- `resolve_version`: when the lookup is not empty, a derived (non-index-served) name without a digest asks the local index first; `Registry` only on a miss.

**C-008 `ocx_index::LocalIndex::import_tags(&self, identifier: &PackageRef, source: &Index, kind: SourceKind, plan: &TagPlan) -> Result<Vec<TagOutcome>>`.**
- Writes rows only for `plan.added` and `plan.replaced`. `unchanged` and `conflict` tags are never written. `replaced` exists only for a package named on the command line.
- `Published`: the `refresh_published` path, `RootScope::Tags(plan.added ∪ plan.replaced)`, catalog cross-check against the export's `c/index.json`.
- `kind` comes from the home's configured sources; a registry none serves is `SourceKind::Derived` (`chained_index.rs:196-207`).
- `Derived`: the `refresh_derived` path.
- An existing root keeps its `repository`. A new root gets `oci://<registry>/<repo>`; the export's pointer is never read into a row.
- Never deletes a row. Returns per tag `added | unchanged | replaced | conflict`, with the home digest on `replaced` and `conflict`.

**C-009 `ocx_index::export::write_export_index(home: &LocalIndex, out: &IndexStore, identifier: &PackageRef, tags: &[String]) -> Result<Vec<TagRow>>`.**
- Reads the root through the home's kind; writes a root with exactly `tags` into `out` with published shape, the dispatch objects those tags name byte for byte, and the catalog entry over the restricted bytes; ensures `config.json`.
- Merges with an existing root in `out`: union of rows; a tag pinned elsewhere is replaced and returned `replaced`.
- The catalog is written after every root of the run.

**C-010 `ocx_package_manager::PackageManager::export(request: ExportRequest) -> Result<ExportOutcome, Error>`.**
```rust
pub struct ExportRequest {
    pub output: PathBuf,
    pub packages: Vec<PackageRef>,          // empty only for the lock form
    pub lock_pins: Vec<LockPin>,            // root export: (repository, platform, leaf digest)
    pub platforms: Vec<Platform>,           // empty = all
    pub referrers: bool,
}
```
- Takes the per-`DIR` scoped lock for the whole run.
- Resolves tags through the ambient chain, as `pull` does, so a first-sight tag is committed to the home index.
- Fills `output/blobs` through home → lookup → network with `BlobWriter`, bounded by `MAX_CONCURRENT_BLOB_TRANSFERS` per leaf and the context `jobs` across packages.
- Refuses a registry whose slug collides with one already in the marker (`export_dir_invalid`).
- Writes `referrers/.complete` once a subject's listing was read in full.
- Writes `output/index` via C-009 for tagged and bare identifiers; nothing for digests and lock pins. Writes `ocx-export.json` last.
- A digest identifier with zero or several platforms is a usage error (64). Offline with a blob nowhere local: `offline_mode` (81).

**C-011 `PackageManager::import(request: ImportRequest) -> Result<ImportOutcome, Error>`.**
- Reads `DIR/ocx-export.json` (C-017); maps every `DIR/index/<dir>` through it; cross-checks named identifiers.
- Builds a view: `ChainMode::Offline`, no client, lookup `with_leading_dir(DIR/blobs)`, auto-verify with `ReferrerSource::LookupOnly` and Sigstore offline.
- Per package: builds the `TagPlan` (a differing home pin is `replaced` when the package was named, `conflict` otherwise), pulls `tag@digest` for `added`, `unchanged` and `replaced` tags on `read_only_view()`, then calls C-008. `conflict` tags materialize nothing and warn once each. A package whose pull or verification failed commits nothing.
- Exit 0 when every package succeeded, `conflict` rows included.
- Digest identifiers: pull by digest, no plan, no pin.
- Checks each root against its catalog entry before planning; a mismatch is `export_dir_invalid`.
- Copies carried referrer manifests, their blobs and markers into the home through C-012, keeping only markers whose manifest is present, digest-checked and names the subject.
- No candidate symlinks, no ledger.

**C-012 Referrer markers (`ocx_oci::blob_lookup::markers`).** `write_marker(root, registry, subject, referrer)`, `write_complete(root, registry, subject)`, `read_markers(root, registry, subject, cap) -> ReferrerMarkers`. Empty files, written atomically, idempotent. The home `BlobStore` calls these with its own root.

**C-013 Verify referrer seam (`ocx_sign`).** `VerifyCtx` gains `lookup: Option<(&Path /*home blobs*/, &BlobLookup)>`, `logical_registry: &str`, and `referrer_source: ReferrerSource { Live, LookupOnly }`.
- `Live`: candidates are the transport listing first, then marker-only digests.
- `LookupOnly`: markers only; the subject manifest, referrer manifests and signature blobs come from home and lookup; the transport is never called.
- Every marker candidate is read and filtered on `subject` and `artifactType` before `MAX_SIGNATURE_CANDIDATES` applies.
- `LookupOnly` with no candidate and `complete == false` names "referrers were not exported" in the `no_signatures_found` message.
- `AutoVerify` gains `with_referrer_source(ReferrerSource)`; import sets `LookupOnly`.

**C-014 Copy source seam (`ocx_oci::copy`).** `Transfer` takes `Option<&BlobLookup>`. Leaf manifest, blobs and referrer listing for the source read the lookup first, keyed by the source identifier's registry as written. `MAX_REFERRERS_PER_LEAF` is replaced by `blob_lookup::MAX_REFERRERS_PER_SUBJECT`. The cosign sidecar sweep is skipped with one warning when the source registry cannot be reached and the leaf came from the lookup.

**C-015 Settings resolution (`ocx_cli::app::blob_settings`).**
```rust
pub struct BlobSettings { pub dirs: Vec<BlobDir>, pub cache_dir: Option<PathBuf> }
pub struct BlobDir { pub path: PathBuf, pub origin: SettingOrigin /* Flag | Env | Config */ }
pub fn resolve(flags: &ContextOptions, env: &EnvSlot, config: &Config, cwd: &Path) -> Result<BlobSettings, BlobSettingsError>;
```
- Dirs: flags if any, else `OCX_BLOB_DIR`, else `config.blobs.dirs` (anchored). Max 32.
- A missing `Flag` dir is `blob_dir_not_found` (79) at resolve time. A missing `Env` or `Config` dir passes resolve and warns once at first use.
- A relative path in the `[managed]` tier is refused at config load (78).
- Cache: flag ▸ env ▸ config; empty env is unset.

**C-016 #759 registration.** `ProjectRegistry::register_if_absent(project_dir) -> Result<Registered, Error>` (`Registered::{Written, AlreadyPresent}`), plus a best-effort wrapper. One `read_link`; a write only when the entry is absent or points elsewhere. Called by `toolchain_exec.rs` after a successful composition, for a project tier only, never `--global`. Fires whether or not anything was fetched. The per-prompt hook does not call it.

**C-017 Export marker (`ocx_oci::blob_lookup::export_marker`).**
```rust
pub struct ExportMarker { pub format_version: u64, pub registries: BTreeMap<String, String> }
pub fn read_marker(dir: &Path) -> Result<Option<ExportMarker>, LookupError>;   // read_bounded, 64 KiB cap
pub fn merge_registry(marker: &mut ExportMarker, registry: &str) -> Result<(), SlugCollision>;
```
- File `ocx-export.json` at an export root or a cache root. `format_version` 1. Absent file means layout 1.
- `merge_registry` refuses a registry whose slug equals another entry's after ASCII case folding.
- `registries` keys are slugs; each must equal `to_relaxed_slug(value)`, else `export_dir_invalid`.
- No `deny_unknown_fields`: a later version may add fields.

**C-018 Provenance gate (`tasks/pull.rs`).** Setup tracks whether the leaf manifest bytes came from the network in this pull. `record_origin` runs only then. A lookup hit, a cache hit and a home hit write no `refs/origins/` file.

**C-019 Root import (`PackageManager::import_project`).** Reads the marker, cross-checks lock entry registries, runs the project pull for the selected groups and platforms on an offline view with `DIR/blobs` leading and `LookupOnly` verification, renders the toolchain home, registers the project (never under `--global`). Writes no index pin.

### Scenarios

Each scenario names the mutation that must turn it red.

| ID | Scenario | Red under |
|---|---|---|
| S-001 | Export one tagged package, all platforms, referrers on: `DIR/index/<slug>/p/<ns>/<pkg>.json` lists exactly that tag; `DIR/blobs` holds the image index, every leaf, config, layer archive, referrer manifest, marker and `.complete`; `ocx-export.json` maps the slug | Root not restricted to exported tags |
| S-002 | Import with the network blocked into an empty home, then `ocx --offline package install pkg:tag`: exit 0, candidate created | Import skipping `import_tags` |
| S-003 | Network blocked. Flip one byte in a layer archive in `DIR/blobs`: import exits 65, `error.detail` = `digest_mismatch` | Bad blob-directory copy not remembered (exit 81) |
| S-004 | Export `a:1` and `b:1`; `import DIR a`: home index has `a` with tag `1` only and no `b` | Names widening instead of narrowing |
| S-005 | Trust policy covering the package: import succeeds offline, no registry request. Same with `--no-referrers` at export: import exits 79, `no_signatures_found`, message names "not exported" | Auto-verify left on `Live` (dials, or 81) |
| S-006 | After import, delete `DIR`, block the network: `ocx package exec pkg:tag -- tool --version` exits 0 | Import leaving layers unextracted |
| S-007 | V7: import a package export, delete it, `ocx exec` in a project whose `ocx.lock` pins the imported digests, `ocx clean`, `ocx --offline exec` exits 0 | Reverting the #759 registration |
| S-008 | Import without install; `ocx clean`: the package directory and its metadata blobs are gone; the index pins remain | Import creating a candidate symlink |
| S-009 | Non-default registry, empty home. Export a package with tags `1`, `2`, bare identifier gives only `latest`; export `pkg:1` and `pkg:2`; `import DIR` pins `1` and `2` | Bare identifier exporting every tag |
| S-010 | Export with `-p linux/arm64` only; import on amd64 with `-p linux/arm64` assembles; host default exits 79 naming `linux/amd64`; later `ocx --offline package install pkg:tag -p linux/amd64` exits 81 naming the leaf | Error without the platform |
| S-011 | Registries `a.test` and `b.test` both serve `team/tool` with different content: both export into separate subtrees and import correctly | Keying blobs by repository |
| S-012 | 8 concurrent `ocx pull` processes, separate homes, one empty cache: all exit 0, every cache `data` verifies, and once all exited 0 `.tmp/` holds no file | `persist` without the occupant check |
| S-013 | Second job: empty home, warm cache, lock-pinned `ocx pull`: the registry access log records zero requests | Tee skipping metadata |
| S-014 | Export with no `-p`: every platform present; `-p linux/amd64 -p darwin/arm64`: exactly those two leaves | Filter ignoring the second value |
| S-015 | `--blob-dir /does/not/exist`: exit 79, `blob_dir_not_found`. `OCX_BLOB_DIR=/does/not/exist ocx clean`: exit 0 | Env dirs checked at resolve time |
| S-016 | A cache entry with a flipped byte: pull warns once, removes it, refetches, exits 0 | Cache mismatch made fatal |
| S-017 | Home pins `pkg:1` to D1; export pins it to D2. `import DIR` (no names) keeps D1, reports `conflict` with both digests, materializes nothing for D2, imports other tags, exits 0. Then `import DIR pkg` moves `pkg:1` to D2, reports `replaced`, materializes D2, exits 0 | `import_tags` writing a `conflict` row (first half), or the plan ignoring the name (second half) |
| S-018 | Export of `Reg.test/x` into a `DIR` holding `reg.test/y`: `export_dir_invalid` naming both | Slug collision not folded |
| S-019 | Import of a `DIR` with no `ocx-export.json`: `export_dir_invalid`; with a marker but no `index/`: `export_dir_invalid` naming `ocx import` | Mapping falling back to configured namespaces |
| S-020 | Network blocked. Truncate a layer archive in `DIR/blobs`: import exits 65, not 75 | Short local read left as `ShortBlobRead` |
| S-021 | Plant a valid blob in the cache, pull it: `refs/origins/` is empty and `verified_sources` returns `None` | `record_origin` ungated |
| S-022 | First-sight import of a published namespace: the new root's `repository` is `oci://<registry>/<repo>`, not the export's pointer. Same for a registry the home does not configure: the root is written as `Derived`, with no catalog cross-check | Root built from the export's root, or kind taken from the export's `config.json` |
| S-023 | Export `pkg:1` filtered to amd64, import, re-export adding arm64 into the same `DIR`, import again: pin `unchanged`, arm64 now materializes | Re-export dropping the amd64 blobs |
| S-024 | Corrupt a root in `DIR/index` after export: import exits 65 `export_dir_invalid` naming "re-run export" | Catalog check skipped |
| S-025 | `ocx export` with an active patch snapshot; target: `ocx import DIR`, delete `DIR`, block the network: `ocx --format json env` equals the connected machine's | Snapshot companions not exported |
| S-026 | `ocx --global export -o DIR`; target: `ocx --global import DIR`: tools run offline; `projects/` gains no entry | Root import registering under `--global` |
| S-027 | `ocx package import DIR reg/repo@sha256:… -p linux/amd64`: content materialized, home index unchanged | Digest import writing a pin |
| S-028 | `ocx --index DIR/index --blob-dir DIR/blobs package copy reg/repo:1 -i mirror/repo:1` with the source registry down: exit 0, one sidecar-skip warning | Tag resolved by a live read |
| S-029 | `ocx exec` in an already registered project: `projects/` mtime and inode unchanged; `ocx package exec` outside a project: `projects/` unchanged | `register` called instead of `register_if_absent` |
| S-030 | Network blocked. Delete one layer archive of an included platform from `DIR/blobs`: import exits 79, `export_not_found`, naming the blob and the package | End-of-sources branch reporting `offline_mode` |
| S-031 | Empty home, registry reachable: `ocx package export -o DIR pkg:1` exits 0, and the home index now pins `pkg:1` to the exported digest | Export resolving without committing the first-sight pin |
| S-032 | Empty home: `ocx --offline package export -o DIR pkg:1` exits 81, `error.detail` = `index_resolution_blocked`, and `DIR` holds no marker | Offline export dialing, or writing the marker before failure |

## 7. CLI grammar and settings

### 7.1 Commands

```
ocx package export -o, --output <DIR> [-p, --platform <PLATFORM>]... [--referrers | --no-referrers] <PACKAGE>...
ocx package import <DIR> [-p, --platform <PLATFORM>]... [<PACKAGE>]...
ocx [--global | --project <PATH>] export -o, --output <DIR> [-g, --group <GROUP>]... [-p, --platform <PLATFORM>]... [--referrers | --no-referrers]
ocx [--global | --project <PATH>] import <DIR> [-g, --group <GROUP>]... [-p, --platform <PLATFORM>]...
```

- `package export` requires `--output` and at least one package (clap, 64).
- Root `export` and `import` read the project's `ocx.lock`; missing `ocx.toml` is 64, missing lock is 78, as `exec`. `-g` defaults to every group on export and to the default group on import, as `ocx pull`.
- `import` takes the export root, not `DIR/index` or `DIR/blobs`.
- **Referrer flags for export.** Copy's `options::Referrers` help names the Referrers API at both ends, sidecar tags and exit 82, which do not apply to export. clap derive fixes help text per struct, so export flattens a new `options::ExportReferrers` (`options/export_referrers.rs`) with the same two flags and `enabled()`. Short help: "Carry the signatures, SBOMs and attestations of each exported manifest (default)" and "Leave them out; a trust policy then cannot verify the import offline".

Short help:
- `package export`: "Write packages and their blobs to a directory for offline transfer"
- `package import`: "Import packages from an export directory without network access"
- `export`: "Write every package the project lock pins to a directory"
- `import`: "Install the project lock's packages from an export directory, offline"

### 7.2 Root flags

| Flag | Env | Config | Default |
|---|---|---|---|
| `--blob-dir <PATH>` (repeatable) | `OCX_BLOB_DIR` (`PathList`, `Public`, `propagation = UserInput`) | `[blobs] dirs` | none |
| `--blob-cache-dir <PATH>` | `OCX_BLOB_CACHE_DIR` (`Path`, `Public`, `propagation = UserInput`) | `[blobs] cache_dir` | none |

### 7.3 Config schema

```toml
[blobs]
dirs = ["/mnt/offline/export/blobs", "relative/to/this/config/file"]
cache_dir = "/var/cache/ocx-blobs"
```

- `BlobsConfig { dirs: Option<Vec<FileReference>>, cache_dir: Option<FileReference> }`, no `deny_unknown_fields`.
- Allowed in every `config.toml` tier, `[managed]` included; managed paths must be absolute (78). Not in `ocx.toml`.
- Tier merge: a later tier replaces `dirs` whole; `cache_dir` scalar override.

### 7.4 Import lookup order

The home, then `DIR/blobs`, then the configured blob directories in list order, then the cache. No network step.

## 8. On-disk layout

### 8.1 Export directory

```
DIR/
  ocx-export.json                                      # {format_version, registries}
  index/<slug(source)>/config.json                     # IndexFormatConfig
  index/<slug(source)>/c/index.json                    # authored over restricted roots, written last
  index/<slug(source)>/p/<ns>/<pkg>.json               # root, exported tags only
  index/<slug(source)>/p/<ns>/<pkg>/o/<alg>/<hex>.json # dispatch objects, verbatim
  blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/data
  blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/digest
  blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/referrers/<alg>-<hex>
  blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/referrers/.complete
  blobs/.tmp/
```

Root `ocx export` writes `ocx-export.json` and `blobs/` only.

```json
{ "format_version": 1, "registries": { "registry.test": "registry.test", "localhost_5000": "localhost:5000" } }
```

### 8.2 Blob cache directory

```
CACHE/
  ocx-export.json                                      # advisory, best effort
  <slug(registry)>/<alg>/<2hex>/<30hex>/data
  <slug(registry)>/<alg>/<2hex>/<30hex>/digest
  .tmp/
```

A cache is itself a valid blob directory, so `--blob-dir CACHE` reads it on another machine. No index documents, ever. Referrer manifests may be cached by digest; markers and listings are not.

## 9. Report schemas (`--format json`)

### 9.1 `export`

Both reports use the shared vocabulary types (`Platform`, `Digest`, `PackageRef`), so the wire shapes match the other reports.

```json
{
  "schema_version": 1,
  "output_dir": "/abs/DIR",
  "packages": [
    {
      "identifier": "registry.test/team/tool:1.2",
      "tags": [{ "tag": "1.2", "digest": "sha256:…", "status": "added" }],
      "manifests": [{ "platform": { "os": "linux", "architecture": "amd64" }, "digest": "sha256:…", "status": "written" }],
      "referrer_count": 2
    }
  ],
  "blob_count": 14,
  "written_size": 52428800
}
```

| Field | Required | Notes |
|---|---|---|
| `schema_version`, `output_dir`, `packages`, `blob_count`, `written_size` | yes | Object root with a nested plural collection; not a list root (see § 9.3) |
| `packages[].identifier` | yes | As named; a lock pin reads `registry.test/team/tool@sha256:…` |
| `packages[].tags` | no | Absent for digest identifiers and lock pins |
| `packages[].tags[].status` | yes | Open enum `added | replaced | unchanged` |
| `packages[].manifests` | yes | One row per exported platform; `platform` is the shared `Platform` object (`os`, `architecture`, optional `variant`), not a string |
| `packages[].manifests[].status` | yes | Open enum `written | present` |
| `packages[].referrer_count` | yes | 0 under `--no-referrers` |

Plain mode: one row per package and platform: `Package`, `Platform`, `Status`.

### 9.2 `import`

A list root (IC-03).

```json
{
  "schema_version": 1,
  "items": [
    {
      "identifier": "registry.test/team/tool",
      "tags": [
        { "tag": "1.2", "digest": "sha256:…", "status": "added" },
        { "tag": "1.1", "digest": "sha256:…", "status": "conflict", "home_digest": "sha256:…" },
        { "tag": "1", "digest": "sha256:…", "status": "replaced", "home_digest": "sha256:…" }
      ],
      "manifests": [{ "platform": { "os": "linux", "architecture": "amd64" }, "digest": "sha256:…", "status": "materialized" }]
    }
  ]
}
```

| Field | Required | Notes |
|---|---|---|
| `schema_version`, `items` | yes | |
| `items[].identifier` | yes | Package import: as listed; root import: the lock pin, e.g. `registry.test/team/tool@sha256:…` |
| `items[].tags` | no | Absent for digest identifiers and root import |
| `items[].tags[].status` | yes | Open enum `added | unchanged | replaced | conflict`. `replaced` only for a named package; `conflict` only when the package was not named |
| `items[].tags[].home_digest` | no | Present only on `replaced` (the digest moved from) and `conflict` (the digest kept) |
| `items[].manifests` | yes | Platforms materialized for `added`, `unchanged` and `replaced` tags; a `conflict` tag adds none |
| `items[].manifests[].status` | yes | Open enum `materialized | present` |

Plain mode: `Package`, `Tag`, `Platform`, `Status`. A `conflict` tag exits 0; it is a report row and a stderr warning, not an error.

### 9.3 Interface-contract fit (IC-03, IC-04)

- **The two roots differ on purpose.** IC-03 makes a *list* report `{schema_version, items}`. `import` is a list: every field is per package, so it uses `items`. `export` carries run-level fields (`output_dir`, `blob_count`, `written_size`) that belong to no package, so it is an object root, and its collection is the plural noun `packages`, as IC-03 requires for nested collections.
- **IC-04.** Every optional field (`tags`, `home_digest`) is omitted when unset. Neither report emits `null`.
- **IC-09.** Every `status` is a named open enum, so a client must tolerate a value it does not know.

## 10. Exit codes and slugs

| Condition | Exit | Slug | New |
|---|---|---|---|
| `--blob-dir` missing or not a directory | 79 | `blob_dir_not_found` | yes |
| Export lacks the registry, package, tag, platform or blob named; at import, a blob no source holds and no blob directory held a bad copy of | 79 | `export_not_found` | yes |
| No marker, unmapped directory, slug collision, catalog and root disagree, shard collision in an export, no `index/` on package import | 65 | `export_dir_invalid` | yes |
| A bad copy in a blob directory and no clean copy anywhere, short blob-directory reads included; holds with network, `--offline` and import alike | 65 | `digest_mismatch` | reused |
| Malformed manifest or over-cap metadata | 65 | `invalid_manifest` | reused |
| Import tag `conflict` (report row and warning) | 0 | — | — |
| A covered package without referrer candidates | 79 | `no_signatures_found` | reused |
| A read that needs the network under `--offline`, cache-only mismatches included (message counts discarded entries) | 81 | `offline_mode` | reused |
| Export of a tag the home does not pin under `--offline` | 81 | `index_resolution_blocked` | reused |
| Digest identifier without exactly one `--platform` on export; missing package argument | 64 | usage | reused |
| Relative `[managed]` blob path | 78 | `blob_path_relative` | yes |
| Write failure into `DIR` or the cache | 74 | I/O | reused |
| An export root document disagrees with its catalog | 65 | `export_dir_invalid` | yes |
| Two digests share one blob directory (shard collision): in an export or import it is `export_dir_invalid`, in a blob cache or blob directory read `shard_collision` | 65 | `shard_collision` | yes |
| Reading or writing a blob directory failed | 74 | `lookup_io` | yes |
| A digest identifier needs exactly one platform to export | 64 | `export_platform_required` | yes |

## 11. Data flows

**Export (`ocx package export -o DIR pkg:1`).** Lock `DIR` → resolve `pkg:1` through the ambient chain (commits first-sight pin) → image index → selected leaves → config, layers, dependency closure, referrers with `.complete`. Each blob: lookup order → `BlobWriter(DIR/blobs)`. Then C-009 writes roots, then the catalog, then the marker. Report.

**Import (`ocx package import DIR pkg`).** Read marker → check roots against catalog → map source dirs → per package: tag plan → pull `tag@digest` on the read-only offline view with `DIR/blobs` leading and `LookupOnly` verify → commit `added` and `replaced` pins → referrers to home → report → exit 0 unless a package failed.

**Root import (`ocx import DIR`).** Read marker → cross-check lock registries → project pull, offline, `DIR/blobs` leading, `LookupOnly` → render → register → report.

**Pull with a blob directory.** As today, with the lookup between the home read and the network at both seams.

**Pull with a cache.** Network fetches tee into the cache; later pulls on other homes read the cache before the network.

## 12. Security (STRIDE, hostile inputs)

| Threat | Vector | Mitigation |
|---|---|---|
| **Spoofing** of a pin | Export pins a tag the user did not mean | Names only narrow; no tag outside the export; a no-names import never moves an existing pin (`conflict`, warning, exit 0); a pin moves only for a package named on the command line (`replaced`). **Residual, stated:** import trusts the exporter as an index source for the tags it adds. Docs recommend `[[trust.policy]]` for an untrusted export. |
| **Spoofing** of routing | Export root carries `oci://evil/…` | Never adopted. New roots get `oci://<registry>/<repo>`; the next `ocx index update` takes routing from the configured source |
| **Spoofing** of a registry | Export maps a directory to a registry the user never named | Mapping only through the marker; named identifiers cross-checked; import never dials |
| **Spoofing** of provenance | Planted cache or export entry passes as a verified source for namespace consent | No `refs/origins/` on a lookup hit (C-018) |
| **Tampering** with blobs | Changed or truncated byte in `DIR` | Digest and completeness check on every read; bad copy remembered, verdict 65 |
| **Tampering** with the cache | Planted or corrupt entry | Verify on read; mismatch removes the entry; occupant verified before `AlreadyPresent`; another digest's entry never overwritten |
| **Tampering** with signatures | Planted referrer markers | Filter on `subject` and `artifactType` before the cap; 256 per root; live listing first; markers never displace live candidates |
| **Tampering**, TOCTOU | `DIR` mutated between check and use | Metadata read once into memory then verified; layers verified as streamed into a temp; open-handle type check |
| **Tampering**, partial export | Interrupted re-export | Per-`DIR` lock; catalog and marker last; import checks root against catalog |
| **Repudiation** | Unsigned content imported | Auto-verify with `LookupOnly`; a covered package without referrers fails 79 |
| **Information disclosure** | Cache readable by other users | Umask default mode; docs: a cache must not cross a trust boundary that private-registry content would cross. Multi-user modes: #760 |
| **Information disclosure** | Export reveals internal hosts and paths | Docs say so. No credential travels: auth stays in the credential store, reports carry no URL with credentials |
| **Denial of service** | Oversized index documents or marker | `FileIndexTransport` cap; dispatch object cap; marker 64 KiB |
| **Denial of service** | Decompression bomb, FIFO or device as `data` | Existing decompressed cap; `O_NONBLOCK` open plus handle check |
| **Denial of service** | Unmounted CI volume in env or config | Warn and skip at use, never at resolve |
| **Denial of service** | Unbounded cache growth | Documented, #760; deleting a cache is always safe |
| **Elevation**, path traversal | `index/` names (`..`, separators) | Names must be marker keys equal to their relaxed slug; `ensure_repository_contained` for `p/` paths |
| **Elevation**, symlinks | Symlinked `index/<dir>` or write ancestors | Symlinked source directories refused; ancestor check before each `create_dir_all`; `remove` never follows a link. Residual race: #760 |
| **Elevation**, env injection | A package sets `OCX_BLOB_DIR` or `OCX_BLOB_CACHE_DIR` | Publish refuses `OCX_*` keys (`validation.rs:186-193`). **Publish-time only:** compose does not filter them from a package pushed by a foreign tool, and the cache directory receives private-registry blobs. Follow-up recorded in `plan_package_export_import.md` § Deferred findings |

## 13. Test map

| Contract | Unit test (crate) | Acceptance |
|---|---|---|
| C-001 | `cas_path::tests::shard_path_unchanged` (`ocx_oci`) | — |
| C-002 | `blob_lookup::tests::{dir_order_wins, bad_dir_copy_continues, bad_dir_copy_verdict_is_65_at_end_of_sources, short_dir_read_is_65, cache_only_mismatch_is_miss, cache_delete_failure_warns, absent_at_import_is_export_not_found, fifo_refused_on_handle, over_cap_refused, tmp_never_read, missing_env_dir_skipped}` | S-003, S-015, S-016, S-020, S-030 |
| C-003, C-004 | `blob_lookup::writer::tests::{noclobber_drops_temp, mismatch_deletes_temp, verified_occupant_short_circuits, corrupt_occupant_replaced, shard_collision_refused, umask_mode, symlinked_ancestor_refused, concurrent_writers_converge}` | S-012 |
| C-005, C-006 | `client::tests::{extract_from_stream_matches_network, tee_writes_only_verified, tee_failure_does_not_fail_pull, retry_uses_fresh_output_dir}` | S-013 |
| C-007 | `chained_index::tests::{lookup_between_home_and_source, offline_consults_lookup, source_fetch_writes_cache}`, `lib::tests::derived_tag_resolves_locally_with_lookup` | S-002, S-028 |
| C-008 | `local_index::tests::{import_tags_writes_added_and_replaced_only, conflict_keeps_home_pin, unconfigured_registry_imports_derived, new_root_gets_identity_pointer, import_never_deletes}` | S-004, S-009, S-017, S-022 |
| C-009 | `export::tests::{root_restricted_to_tags, catalog_reauthored_last, reexport_replaces_moved_tag, dispatch_object_verbatim}` | S-001, S-009, S-023 |
| C-010 | `tasks::export::tests::{digest_needs_one_platform, offline_missing_archive_is_81, offline_unknown_tag_is_81, first_sight_tag_commits_home_pin, slug_collision_refused, marker_written_last}` | S-001, S-011, S-014, S-018, S-031, S-032 |
| C-011 | `tasks::import::tests::{never_dials, no_marker_is_invalid, content_before_pins, failed_package_commits_nothing, conflict_materializes_nothing_exits_zero, named_package_replaces, missing_platform_named, catalog_mismatch_invalid}` | S-002, S-006, S-008, S-010, S-017, S-019, S-024, S-027, S-030 |
| C-012, C-013 | `verify::tests::{markers_filtered_before_cap, live_listing_first, lookup_only_never_lists_live, incomplete_listing_named}` | S-005 |
| C-014 | `copy::tests::{digest_source_from_lookup, sidecar_skipped_with_warning}` | S-028 (`test/tests/test_package_copy_from_export.py`) |
| C-015 | `blob_settings::tests::{flag_replaces_env, config_anchored, missing_flag_dir_is_79, missing_env_dir_passes, managed_relative_refused, windows_quoted_entry}` | S-015 |
| C-016 | `registry::tests::{register_if_absent_no_write_when_present, rewrites_when_pointing_elsewhere}`, `toolchain_exec` regression per #759 | S-007, S-029 |
| C-017 | `export_marker::tests::{absent_is_layout_1, case_fold_collision, slug_must_match_value, unknown_fields_tolerated}` | S-018, S-019 |
| C-018 | `pull::tests::lookup_hit_records_no_origin` | S-021 |
| C-019 | `tasks::import::tests::{root_import_registers_project, global_root_import_registers_nothing}` | S-025, S-026 |

Acceptance modules: `test/tests/test_package_export_import.py`, `test/tests/test_blob_cache.py`, `test/tests/test_project_export_import.py`, `test/tests/test_package_copy_from_export.py`, `test/tests/test_exec_registers_project.py` (#759). Each carries a `command` marker; root `export` and root `import` get a smoke test each.

## 14. Implementation outline for `/hex-plan`

File-disjoint work packages. Contract-first: WP-1 and WP-2 land stubs everyone compiles against. A spike is a throwaway check whose result is written into this file before the work package's stub commit.

| WP | Owns | Spike | Depends on |
|---|---|---|---|
| **WP-1 Contracts** | `crates/ocx_env/src/vars.rs` (two variables), `crates/ocx_config/src/blobs.rs` (new) + `lib.rs` field, `crates/ocx_cli/src/app/context_options.rs` (two flags), `crates/ocx_cli/src/app/blob_settings.rs` (new, C-015) | — | — |
| **WP-2 Lookup, writer, markers** | `crates/ocx_oci/src/cas_path.rs` (new, C-001), `crates/ocx_oci/src/blob_lookup.rs` + `blob_lookup/{writer,stream,markers,export_marker}.rs` (C-002…C-004, C-012, C-017), `crates/ocx_store/src/file_structure/blob_store.rs` and `cas_path.rs` call sites | `tempfile::Builder` permissions honour the umask on the pinned version | WP-1 |
| **WP-3 Layer pipeline and tee** | `crates/ocx_oci/src/client.rs` (C-005, C-006) | The pipeline body accepts an `AsyncRead` file source in place of the transport stream with no change to its cap and drain logic | WP-2 |
| **WP-4 Copy seam** | `crates/ocx_oci/src/copy.rs`, `crates/ocx_cli/src/command/package_copy.rs` (C-014) | Keep-tag write needs nothing from the source (`ocx_package/src/publisher/copy.rs` past line 239) | WP-2 |
| **WP-5 Index seams** | `crates/ocx_index/src/{chained_index.rs,lib.rs,local_index.rs}`, `crates/ocx_index/src/export.rs` (new) (C-007…C-009) | A restricted root reads through `refresh_published` over `FileIndexTransport`; the identity pointer passes `guard_physical_dial` for a published namespace | WP-2 |
| **WP-6 Verify seam** | `crates/ocx_sign/src/verify/{pipeline.rs,candidates.rs}`, `crates/ocx_package_manager/src/tasks/auto_verify.rs` (C-013) | Every subject-manifest and referrer read site in `pipeline.rs` beyond `:424` and `:1544` | WP-2 |
| **WP-7 Tasks** | `crates/ocx_package_manager/src/tasks/{export.rs,import.rs}` (new), `tasks/pull.rs` layer seam and origin gate, `lib.rs` field, `error.rs` slugs (C-010, C-011, C-018, C-019) | — | WP-3, WP-5, WP-6 |
| **WP-8 CLI** | `crates/ocx_cli/src/command/{package_export.rs,package_import.rs,export.rs,import.rs}` (new), `command.rs`, `command/package.rs`, `command/index_common.rs` (constant re-point), `options/export_referrers.rs` (new), `api/data/{export.rs,import.rs}` (new), `app/context.rs` wiring, `command/contract.rs` | — | WP-7, WP-4 |
| **WP-9 #759** | `crates/ocx_project/src/registry.rs` (C-016), `crates/ocx_cli/src/command/toolchain_exec.rs`, `test/tests/test_exec_registers_project.py`; own `fix:` commit referencing #759, regression tests first | — | — |
| **WP-10 Acceptance** | the five acceptance modules in § 13, `test/SUITE_FLOOR` | — | WP-8, WP-9 |
| **WP-11 Contract counterparts and docs** | schema goldens, `cli.json`, `reference/**` regeneration, `website/src/content/docs/guides/work-offline.mdx` section (cache safety, trust boundary, host disclosure, trust policy for untrusted exports) | — | WP-8 |

DAG: WP-1 → WP-2 → {WP-3, WP-4, WP-5, WP-6} → WP-7 → WP-8 → {WP-10, WP-11}. WP-9 is independent and lands first, as its own fix.

## 15. Deferred findings

| Finding | Reason deferred |
|---|---|
| Cosign sidecar tags and `__ocx.desc` in an export | Both are tags; an export holds none. Referrers carry the signatures that verify |
| `ocx env`, `ocx shell`, `ocx direnv export` registration | Scoped out of WP-9; stays on #759 |
| Cache eviction, network file systems, permissions, orphan temp cleanup, ancestor-check race | [ocx-sh/ocx#760](https://github.com/ocx-sh/ocx/issues/760) |
| `--format oci-layout` export (option A′) for oras and skopeo | Interop route, out of scope |
| Per-invocation switch to disable a config-set cache | YAGNI until asked |
| Published-root signature, if `index.ocx.sh` ever signs roots | A restricted root would then need a re-sign story |

## 16. Claims not verified in code

- Whether the `--offline` context builds its `ChainedIndex` with no sources. The `kind_for_registry` doc comment suggests so ("e.g. `--offline`"). If it does, import must read the kind from the configured source list, not from its offline view (WP-5 spike).

- That every metadata read of `ChainedIndex` goes through the content-store seam (only `fetch_blob` at `chained_index.rs:771-826` was read).
- How `refresh_published` reads dispatch objects through a `FileIndexTransport` source for a restricted root (WP-5 spike).
- That the identity pointer `oci://<registry>/<repo>` passes `guard_physical_dial` for an index-served namespace (WP-5 spike).
- That `tempfile::Builder` permissions apply before the umask on the pinned version (WP-2 spike).
- That the layer pipeline body can take a file source unchanged (WP-3 spike).
- Every verify read site that `LookupOnly` must cover (WP-6 spike).
- That the keep-tag write needs nothing from the source (WP-4 spike; `publisher/copy.rs:239` seen, the write not read).
- Whether compose filters a reserved `OCX_*` key from an already-published package; `validation.rs:186-193` is publish time only.
- The acceptance-criteria text of [ocx-sh/ocx#759](https://github.com/ocx-sh/ocx/issues/759); the issue was not fetched.
