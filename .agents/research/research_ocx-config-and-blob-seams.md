# Research: where an extra data directory and a cache directory would attach in OCX

## Metadata

**Date:** 2026-11-22
**Lane:** codebase recon (`/hex-discuss`, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** read-only reading of the worktree at branch `uno`; line numbers as of that date.
**Expires:** 2027-02-22
**Caveat:** findings only, no design. Not read: the `PathList` separator parser, the `MAX_METADATA_BLOB_BYTES` definition, the declaration line of `OCX_INSECURE_REGISTRIES`.

## Config file

- Top-level sections today: `registry`, `registries`, `mirrors`, `patches`, `managed`, `update`, `trust`, `shell`, `records`, plus the root keys `toolchain_dir`, `extra_ca_certs`, `extra_ca_certs_pem` (`crates/ocx_config/src/lib.rs:82-201`). An unknown key is ignored (`lib.rs:72-80`).
- A path is a plain string: `toolchain_dir: Option<PathBuf>` (`lib.rs:170`), `extra_ca_certs: Option<PathBuf>` (`lib.rs:180`). A relative `extra_ca_certs` is anchored at the declaring config file's directory (`lib.rs:365-371`).
- Lists: `registries.<ns>.trusted_hosts: Option<Vec<String>>` (`registry.rs:36`), `shell.profiles: Option<Vec<PathBuf>>` (`shell.rs:63`), `[[trust.policy]]` (`lib.rs:143`).
- Tier merge is per field. A plain list replaces (`registry.rs:106-108`, `shell.rs:119-122`). `[[trust.policy]]` is the one list that appends (`crates/ocx_trust/src/lib.rs:60`). A system-tier entry is locked (`registry.rs:97`).

## Environment variables

- Value kinds: `Bool, TriBool, String, Integer, Path, PathList, PathOrPem, HostList, Choice, Json` (`crates/ocx_env/src/lib.rs:41-55`).
- `OCX_CONSENT_PATHS` is a `PathList`, "separated like `PATH`" (`vars.rs:285-290`). `OCX_INSECURE_REGISTRIES` is a comma-separated host list (`vars.rs:133`).
- `OCX_HOME` is `Path`, public, propagation `SetupWritten` (`vars.rs:28`).
- `OCX_INDEX` is `Path`, public, propagation `UserInput`. Its doc says it "swaps the whole local index collection ... never a partial overlay" (`vars.rs:116-124`).

## `--index` wiring

- `crates/ocx_cli/src/app/context.rs:309-314` builds one `IndexStore` from the override, or the home's. It replaces the home index.
- There is exactly one `LocalIndex` (`context.rs:317-328`) and one `Index::from_chained_with_content_store` (`context.rs:345-350`). No code path reads two local index directories.
- A `file://` index base exists as a network-free source (`file_transport.rs:165`, `registry.rs:16`). That is a source, not a second local copy.

## Blob read seam

- The `BlobStore` is attached at `context.rs:349`, through `crates/ocx_index/src/lib.rs:194-201` and `ChainedIndex::with_content_store` (`chained_index.rs:191`).
- `ChainedIndex::fetch_blob` (`chained_index.rs:771-826`) does the cache-first read (`:776`, digest check `:778`), heals a corrupt copy (`:807`) and writes through (`:809`).
- The key is the logical registry: `BlobStore::path` slugifies `registry` and adds the CAS shard (`blob_store.rs:54-58`).
- `read_blob` reads the whole file into memory with no cap (`blob_store.rs:124-131`). `write_blob` takes `&[u8]` (`:74`), and `persist_bytes` copies it again (`:101`).

## Layer fetch seam

- `pull_layer` (`crates/ocx_oci/src/client.rs:846`) calls `pull_layer_with_caps` (`:878`). The network read is `self.transport().pull_blob_streaming(&image, &layer_digest)` at `:916`. The caller is `crates/ocx_package_manager/src/tasks/pull.rs:858`.
- Pipeline: `raw_stream.take(blob_total_size)` (`:921`), `HashingAsyncReader` (`:923`), `ProgressReader` (`:924`), then decoder, `SyncIoBridge` and tar extraction.
- A local file could replace the stream at `:916`. `OciTransport` is the abstraction and it is sealed.
- The compressed bytes pass one point, between `take` and `HashingAsyncReader` (`:921-923`). A second writer could read there. Nothing writes a compressed layer to disk today.
- A second whole-blob reader is `fetch_layer_blob_capped` (`client.rs:1612-1640`).

## Size assumptions

- Caps are applied by the caller per blob kind: 32 MiB for manifests and index documents (`client.rs:47`, applied after the read at `:1531`), 4 MiB for config blobs, `max(256 MiB, 100 × layer.size)` decompressed for a layer (`client.rs:866-867`).
- `read_bounded` is not used on `blobs/`.
- Garbage collection does not read blob contents. It canonicalises descriptor paths (`garbage_collection.rs:49-53`) and walks directories (`blob_store.rs:144-149`).

## `ocx package copy`

- The source is a registry only, through `OciTransport`. `copy_leaf` is at `crates/ocx_oci/src/copy.rs:116`, and the source blob is fetched with `transport.pull_blob_streaming` at `copy.rs:382`. A directory source would have to be an `OciTransport`.

## CI caching today

- None. Every `setup-ocx` use in `.github/workflows/*.yml` passes `cache: false` (for example `acceptance.yml:124`, `basic-repo.yml:91`). No `actions/cache` appears under `.github`. The only CI cache is the Bazel remote cache (`.github/actions/bazel-cache-rc`).
