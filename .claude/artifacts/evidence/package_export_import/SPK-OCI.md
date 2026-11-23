# SPK-OCI — blob writes and the layer pipeline (SP-4, SP-5, SP-7)

Base: `c2db3902`. Prototype code deleted; only this file is committed.

## SP-4 — `tempfile::Builder::permissions` applies the mode at create time

- **Claim:** on tempfile 3.27.0, `Builder::permissions` sets the mode when the file is created, so the blob writer needs no hand-rolled `OpenOptions`.
- **Method:** `Cargo.lock` pins `tempfile 3.27.0`. Read `tempfile-3.27.0/src/file/imp/unix.rs:15-26` (`create_named`). Then a throwaway binary that creates three files with `0o666` and `stat`s them under umask 022 and 077 (the code of this binary is not kept).
- **Result: holds, with one precision.** `create_named` does `open_options.mode(permissions.map(|p| p.mode()).unwrap_or(0o600))` before `open()` (`unix.rs:23`). The mode goes to `open(2)`, so the umask still masks it: it is the same as a hand-rolled `create_new(true).mode(0o666)`, not umask-independent. The default (no `permissions`) is `0o600`.
- **Evidence (test output):**
  ```
  umask 22: Builder::permissions(0666) -> 644
  umask 22: Builder default -> 600
  umask 22: create_new+mode(0666) -> 644
  umask 77: Builder::permissions(0666) -> 600
  umask 77: Builder default -> 600
  umask 77: create_new+mode(0666) -> 600
  ```
  Both outcomes shown: default `600` vs `permissions(0o666)` `644` (umask 022); umask 077 masks both to `600`.
- **Fallback taken: no.** The plan's hand-rolled `create_new` + random name + `AlreadyExists` retry is not needed; `Builder` already does create_new with a random name and retries on collision.
- **A0 stub shape:** `tempfile::Builder::new().permissions(std::fs::Permissions::from_mode(0o666)).tempfile_in(<store>/.tmp)` under `cfg(unix)` (`std::os::unix::fs::PermissionsExt`); on non-unix, `Builder::new().tempfile_in(..)` unchanged. Result is `0o666 & !umask` (a blob a sibling user's umask 022 makes `0o644`), never wider than a normal `File::create`. A test that needs a fixed mode must set the umask itself or `set_permissions` after; do not assert `0o644` blindly. `NamedTempFile` already owns cleanup (no `TempPath::from_path` needed).

## SP-5 — the layer pipeline takes a file source unchanged

- **Claim (F12):** the pipeline after `.take()` at `client.rs:921` can consume a file source.
- **Method:** read `crates/ocx_oci/src/client.rs:878-1062` (`pull_layer_with_caps`) and the transport signature `pull_blob_streaming -> Result<Box<dyn AsyncRead + Send + Unpin + 'static>>` (`client.rs:3409`, `testing.rs:321`).
- **Result: holds as a pure extraction refactor, and the `ReaderStream` adapter in the plan is not needed.** The transport stream is already a bare `AsyncRead + Send + Unpin + 'static` (`take` at `:921` is `AsyncReadExt::take`; `HashingAsyncReader<R: AsyncRead + Unpin>` at `hashing_reader.rs:84`; `ProgressReader`, `XzDecoder/GzipDecoder/ZstdDecoder(BufReader<_>)`, `SyncIoBridge` all only need `AsyncRead + Unpin` plus `Send + 'static` for `spawn_blocking`). `tokio::fs::File` satisfies all of these, so it can be passed as-is.
- **Network-bound lines (the only ones not generic):** `ensure_auth` (`:893`) and `pull_blob_streaming` (`:913`); `bar` / `on_progress` (`:905-907`) are built before the stream. Everything from `:921` to the end (`:1062`) touches no client state except `self.progress` (the bar, taken before) and error mapping.
- **A0 stub shape:** in `client.rs`, split at `:921` into a private `async fn extract_layer_from_reader<S: AsyncRead + Unpin + Send + 'static>(source: S, layer_digest: &Digest, layer: &Descriptor, output_dir: &Path, blob_total_size: u64, decompressed_cap: u64, on_progress: ProgressFn, label: &str) -> Result<(), ClientError>` that owns `take(blob_total_size)`, the hash/progress wrap, the three compression arms, the cap/short-read/digest/extract checks (order unchanged). `pull_layer_with_caps` keeps media-type check, auth, bar, `pull_blob_streaming` and calls it. The export-side caller (import) calls the same function with `tokio::fs::File::open(blob)`; expose it `pub(crate)` or as a `Client` method per A0's crate seam. The three compression arms monomorphize per source type (two instantiations, `Box<dyn ..>` and `File`): accepted.
- **Open fact for the implementer (F12 residue):** failure leaves `output_dir` behind (comment at `:1038`, "left for the caller's TempStore"); the file path must keep that contract, retries recreate the directory.

## SP-7 — the keep-tag write needs nothing from the source

- **Claim:** `publisher/copy.rs:285-325` keep-tag write reads nothing from the source.
- **Method:** read `crates/ocx_package/src/publisher/copy.rs:285-325` and `Client::push_keep_tag` (`client.rs:690-731`).
- **Result: holds.** Inputs are `request.target`, the merged index returned by `merge_platform_into_index` (a target-side read-modify-write), and `platform`. `push_keep_tag` derives the tag from the platform manifest digest inside the merged index (`:696-701`), then **pulls the manifest by digest from the target** (`pull_manifest_raw(&digest_ref ...)`, `:722`; `digest_ref` is `source_identifier` = the target, `:718`) and pushes it back under the keep tag (`:728`). The name `source_identifier` is the target at the call site (`copy.rs:318`). Because the leaf manifest was pushed to the target earlier in the same run, the read is served from the target registry.
- **Fallback taken: no.** No edit to `crates/ocx_package/src/publisher/copy.rs` is needed for the keep tag; the lookup-by-digest fallback is unreachable on this path. P-COPY needs only `crates/ocx_oci/src/copy.rs` and the CLI file.
- **A0 stub shape:** none for the keep tag; leave `publisher/copy.rs` as it is.
