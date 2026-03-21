# Plan: Transfer Progress Bars (Push & Pull)

## Overview

**Status:** Draft
**Date:** 2026-03-21
**Depends on:** `sion` branch merge (commit `1df718d` — configurable push chunk size)

## Objective

Add byte-level progress bars to push and pull operations, showing the package name and transfer progress in human-readable units (kB, MB). Progress bars display only when stderr is a TTY. When no TTY is present, periodic `tracing::debug!` events provide visibility into stalled transfers in debug logs.

## Scope

### In Scope

- Download progress bar during `pull_blob_to_file` (the package archive blob)
- Upload progress bar during `push_blob` (the package archive blob)
- Human-readable decimal byte formatting (kB, MB, GB — matches cargo/docker)
- Per-package span naming
- Periodic `debug!` log events for non-TTY environments (detect stalled transfers)
- Feature-gated behind `#[cfg(feature = "progress")]`
- TTY-only display (already handled by existing `ProgressMode::detect()`)

### Out of Scope

- Config blob progress (metadata is tiny, <1kB)
- Manifest push/pull progress (also tiny)
- Cascade tag operations (manifest copies, no blob transfer)
- Transfer speed / ETA display (can be added later via indicatif template)
- Extraction progress (fast local operation, not I/O bound)

## Research: How Other CLIs Handle This

| Tool | Style | Units | Non-TTY | Channel |
|------|-------|-------|---------|---------|
| cargo | `[==>] 28.0 MB` aggregate | decimal MB | Silent | stderr |
| docker pull | Per-layer `[==>] 192.8 MB/255.2 MB` | decimal MB | Plain text lines | stderr |
| ORAS | Per-blob rows, aligned columns | SI KB/MB | `--no-tty` disables | stderr |
| curl | `######  30.2%` or multi-column table | decimal | Suppressed | stderr |

**Industry consensus:**
1. Always stderr, never stdout
2. Suppress automatically in non-TTY (no flag needed)
3. Per-item progress for OCI pulls (docker, ORAS pattern)
4. Decimal units (MB, not MiB)

**OCX already does #1 and #2 correctly** via `ProgressMode::detect()` → `init_progress()` which falls back to plain `init()` (no indicatif layer) when stderr isn't a TTY. The `#[cfg(feature = "progress")]` feature gate ensures progress code compiles out when not needed.

## Technical Approach

### Download Progress: ProgressWriter wrapper

The oci-client's `pull_blob` accepts any `T: AsyncWrite`. Currently OCX passes a raw `tokio::fs::File`. By wrapping the file in a `ProgressWriter<T>` that implements `AsyncWrite`, we can intercept every `write` call and update the progress bar — tracking real bytes written to disk.

```
download_to_temp()
  ├─ pull manifest (get blob_layer.size → total bytes known)
  │
  ├─ info_span!("Downloading", package = %identifier)
  │   pb_set_length(blob_layer.size)
  │   pb_set_style(byte_bar_style())
  │
  └─ transport.pull_blob_to_file_with_progress(image, digest, path, size, on_progress)
       └─ NativeTransport:
            let file = File::create(path)
            let writer = ProgressWriter::new(file, on_progress)
            client.pull_blob(image, digest, writer)    ← bytes tracked as written
```

**Why this is accurate:** `pull_blob` calls `out.write_all(&bytes)` for each chunk received from the HTTP response stream. The `ProgressWriter` sees every byte as it's written. Progress matches actual disk writes.

### Upload Progress: Stream wrapper via `push_blob_stream`

The oci-client exposes a public `push_blob_stream` that accepts `Stream<Item = Result<Bytes>>` and pushes chunks via sequential HTTP PATCHes. By splitting data into chunks and tracking when each chunk is consumed, we get accurate upload progress.

The chunk size matches OCX's configured push chunk size (16 MiB default, configurable via `ClientBuilder::push_chunk_size`). The `NativeTransport` reads this from `self.client`'s config to keep chunking consistent.

```
push_image_manifest()
  ├─ read file, calculate digest
  │
  ├─ info_span!("Uploading", package = %identifier)
  │   pb_set_length(package_data_len)
  │   pb_set_style(byte_bar_style())
  │
  └─ transport.push_blob_with_progress(image, data, digest, size, on_progress)
       └─ NativeTransport:
            check blob existence (skip if exists)
            split data into CHUNK_SIZE Bytes chunks (16 MiB default)
            create unfold stream:
              .next() #1: yield chunk_0                     (position = 0)
              .next() #2: on_progress(16MB), yield chunk_1  ← chunk_0 uploaded
              ...
              .next() returns None
            client.push_blob_stream(image, stream, digest)
            on SpecViolationError: fallback to push_blob (no progress)
            on_progress(total)
```

**Why this is accurate:** `push_blob_stream` calls `.next()` only after the previous chunk's HTTP PATCH completes. Updating position in `.next()` reflects confirmed uploads, lagging by at most one chunk.

### Non-TTY: Debug Log Events

When stderr is not a TTY, progress bars are suppressed (no indicatif layer). To provide visibility into stalled transfers in CI/scripts, both `ProgressWriter` and the upload stream emit periodic `tracing::debug!` events.

**Pattern** (same as existing `archive/tar.rs` pattern with `LOG_INTERVAL`):

```rust
// ProgressWriter (download):
// After each write, check if we crossed a reporting threshold
if self.bytes_written / LOG_BYTE_INTERVAL > self.last_logged / LOG_BYTE_INTERVAL {
    tracing::debug!("Downloaded {} / {} bytes", self.bytes_written, self.total);
    self.last_logged = self.bytes_written;
}

// Upload stream (push):
// After each chunk, emit debug log
tracing::debug!("Uploaded {} / {} bytes", bytes_sent, total);
```

`LOG_BYTE_INTERVAL` — emit a `debug!` event every N bytes transferred. A reasonable interval is every 32 MiB (`32 * 1024 * 1024`), producing ~3 log lines for a 100 MB package. Defined as a constant in `cli/progress.rs` alongside the existing `LOG_INTERVAL` for file counts.

These events are always emitted (not feature-gated) but only visible when the user sets `-l debug` or `CONSOLE=debug`. This matches the existing pattern in `archive/tar.rs` where `tracing::debug!("Bundled {} entries", count)` fires every `LOG_INTERVAL` entries.

### Shared Infrastructure

Both directions use the same `byte_bar_style()` and tracing span pattern:

```rust
// cli/progress.rs
pub fn byte_bar_style() -> indicatif::ProgressStyle {
    indicatif::ProgressStyle::default_bar()
        .template("{span_child_prefix}{spinner} {span_name} [{bar:30}] {bytes}/{total_bytes}")
        .expect("valid progress template")
        .progress_chars("=> ")
}
```

Renders as: `⠋ Downloading cmake:3.28 [=====>           ] 12.5 MB/45.2 MB`

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| No submodule changes | Use public APIs only: `pull_blob` with `AsyncWrite` wrapper, `push_blob_stream` with progress stream. |
| `ProgressWriter<T: AsyncWrite>` for downloads | Intercepts `write_all` calls inside `pull_blob`. Tracks real bytes written. Zero-copy — just forwards to inner writer. |
| `push_blob_stream` + `unfold` for uploads | Uses oci-client's public streaming API. Each `.next()` call happens after the previous chunk was uploaded. |
| Use configured chunk size (16 MiB), not hardcoded 4 MiB | OCX overrides oci-client's default 4 MiB to 16 MiB (`ClientBuilder`, sion branch). The upload stream must use the same chunk size to avoid double-chunking inside `push_blob_stream`. |
| Expose chunk size from `NativeTransport` | `NativeTransport` needs access to `push_chunk_size` from the oci-client `ClientConfig` to split data correctly. Either store it during construction or read from `self.client`. |
| Fall back to `push_blob` on `SpecViolationError` | `push_blob_stream` only does chunked push. Some registries need monolithic. On error, fall back without progress. |
| `{bytes}/{total_bytes}` template | indicatif's built-in byte formatting. Decimal (1000-based: kB, MB, GB) like cargo and docker. |
| Span naming: "Downloading" / "Uploading" | Distinct from existing "Installing" / "Resolving" spans. Clear about what's happening at each phase. |
| Periodic `debug!` for non-TTY | Detect stalled transfers in CI logs without polluting normal output. Matches existing `archive/tar.rs` pattern. |
| Progress methods on `OciTransport` with default impls | Default delegates to non-progress methods. `StubTransport` uses defaults. Only `NativeTransport` overrides. |

### TTY Handling (Already Correct)

The existing architecture handles this properly:

1. `ProgressMode::detect()` checks `console::Term::stderr().is_term()`
2. `LogSettings::init_progress(style)` conditionally adds `IndicatifLayer` only for TTY
3. When not TTY → plain `init()` → no indicatif layer → spans produce no visual output
4. `#[cfg(feature = "progress")]` compiles out all `pb_set_*` calls when feature is off

**No additional TTY logic needed.** The progress bars are invisible by design in non-TTY contexts. Debug log events provide non-TTY visibility independently.

## Implementation Steps

### Phase 1: ProgressWriter for downloads

- [ ] **Step 1.1:** Create `ProgressWriter<W>` struct
  - File: `crates/ocx_lib/src/oci/client/progress_writer.rs`
  - Generic over `W: AsyncWrite`
  - Fields: `inner: Pin<Box<W>>`, `on_progress: Box<dyn Fn(u64) + Send + Sync>`, `bytes_written: u64`, `total: u64`, `last_logged: u64`
  - Implements `AsyncWrite`: delegates `poll_write` / `poll_flush` / `poll_shutdown` to inner
  - After each successful `poll_write`, calls `on_progress(bytes_written_so_far)`
  - Emits `tracing::debug!` every `LOG_BYTE_INTERVAL` bytes (not feature-gated)

- [ ] **Step 1.2:** Add `pull_blob_to_file_with_progress` to `OciTransport` trait
  - File: `crates/ocx_lib/src/oci/client/transport.rs`
  - Signature: `async fn pull_blob_to_file_with_progress(&self, image, digest, path, total_size: u64, on_progress: &(dyn Fn(u64) + Send + Sync)) -> Result<()>`
  - Default impl: delegates to `pull_blob_to_file` (ignores progress)

- [ ] **Step 1.3:** Implement in `NativeTransport`
  - File: `crates/ocx_lib/src/oci/client/native_transport.rs`
  - Opens file, wraps in `ProgressWriter::new(file, total, on_progress)`, calls `self.client.pull_blob(image, digest, writer)`

### Phase 2: Progress stream for uploads

- [ ] **Step 2.1:** Add `push_blob_with_progress` to `OciTransport` trait
  - File: `crates/ocx_lib/src/oci/client/transport.rs`
  - Signature: `async fn push_blob_with_progress(&self, image, data: Vec<u8>, digest, on_progress: &(dyn Fn(u64) + Send + Sync)) -> Result<String>`
  - Default impl: delegates to `push_blob` (ignores progress)

- [ ] **Step 2.2:** Expose push chunk size on `NativeTransport`
  - File: `crates/ocx_lib/src/oci/client/native_transport.rs`
  - Store `push_chunk_size: usize` during `NativeTransport::new()`, reading it from the `ClientConfig` passed to the oci-client. Or: add a constant `const PUSH_CHUNK_SIZE: usize = 16 * 1024 * 1024` matching the value in `builder.rs`.

- [ ] **Step 2.3:** Implement `push_blob_with_progress` in `NativeTransport`
  - File: `crates/ocx_lib/src/oci/client/native_transport.rs`
  - Check blob existence (same as `do_push_blob`)
  - Split `Vec<u8>` into `push_chunk_size` `Bytes` chunks
  - Create `futures::stream::unfold` that yields chunks, calls `on_progress` with cumulative bytes, and emits `debug!` per chunk
  - Call `self.client.push_blob_stream(image, stream, digest)`
  - On `SpecViolationError`: log warning, fall back to `self.client.push_blob(image, data, digest)`
  - Final `on_progress(total)` to ensure 100%

### Phase 3: Byte bar style and log constants

- [ ] **Step 3.1:** Add `byte_bar_style()` and `LOG_BYTE_INTERVAL` to `cli/progress.rs`
  - File: `crates/ocx_lib/src/cli/progress.rs`
  - `byte_bar_style()`: template with `{bytes}/{total_bytes}`
  - `LOG_BYTE_INTERVAL: u64 = 32 * 1024 * 1024` (32 MiB — debug log every ~32 MB)

### Phase 4: Wire progress into download path

- [ ] **Step 4.1:** Add download progress span to `download_to_temp`
  - File: `crates/ocx_lib/src/oci/client.rs`
  - After manifest pull (line 279), before blob pull (line 322):
    1. Get total size from `blob_layer.size` (already available as `i64`)
    2. Create `info_span!("Downloading", package = %identifier)`
    3. `#[cfg(feature = "progress")]`: Set `pb_set_length` and `pb_set_style`
    4. Create progress closure: `|bytes| span.pb_set_position(bytes)`
    5. Call `transport.pull_blob_to_file_with_progress(image, digest, path, size, &progress_fn)` for the package blob
    6. Config blob continues using regular `pull_blob_to_file` (too small for progress)

### Phase 5: Wire progress into upload path

- [ ] **Step 5.1:** Add upload progress span to `push_image_manifest`
  - File: `crates/ocx_lib/src/oci/client.rs`
  - After file read and digest calculation (line 443), before blob push (line 446):
    1. Create `info_span!("Uploading", package = %package_info.identifier)`
    2. `#[cfg(feature = "progress")]`: Set `pb_set_length(package_data_len as u64)` and `pb_set_style`
    3. Create progress closure
    4. Call `transport.push_blob_with_progress(image, data, digest, &progress_fn)` for package blob
    5. Config blob continues using regular `push_blob`

### Phase 6: Verification

- [ ] **Step 6.1:** Run `task verify`
- [ ] **Step 6.2:** Manual test: push to localhost:5000, verify progress bar renders
- [ ] **Step 6.3:** Manual test: pull a package, verify progress bar renders
- [ ] **Step 6.4:** Test non-TTY: pipe output, confirm no progress bars but debug logs work
  - `ocx install cmake:3.28 2>&1 | cat` — no progress bars
  - `ocx -l debug install cmake:3.28 2>&1 | cat` — debug lines show transfer progress

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `crates/ocx_lib/src/oci/client/progress_writer.rs` | Create | `ProgressWriter<W: AsyncWrite>` with progress callback + debug logging |
| `crates/ocx_lib/src/oci/client/transport.rs` | Modify | Add `pull_blob_to_file_with_progress` and `push_blob_with_progress` with default impls |
| `crates/ocx_lib/src/oci/client/native_transport.rs` | Modify | Override both new methods: `ProgressWriter` for pull, `push_blob_stream` for push |
| `crates/ocx_lib/src/oci/client.rs` | Modify | Add progress spans in `download_to_temp` and `push_image_manifest`; add `mod progress_writer` |
| `crates/ocx_lib/src/cli/progress.rs` | Modify | Add `byte_bar_style()` and `LOG_BYTE_INTERVAL` |

## Dependencies

| Package | Status | Purpose |
|---------|--------|---------|
| `futures` | Already in deps | `futures::stream::unfold` for upload progress stream |
| `bytes` | Already in deps | `Bytes::split_to` for zero-copy chunking |
| `indicatif` | Already in deps (optional) | `{bytes}/{total_bytes}` template formatting |
| `pin-project-lite` | May need to add | For `pin_project!` in `ProgressWriter` (or use manual `Pin<Box<W>>`) |

## Prerequisites

This plan assumes the `sion` branch changes are merged to `main` first:
- `1df718d perf(oci): increase default push chunk size to 16 MiB` — adds `push_chunk_size` to `ClientConfig` (submodule) and `ClientBuilder` (OCX), defaulting to 16 MiB.

Without this merge, the upload stream would need to hardcode a chunk size constant. The plan works either way, but using the configured value is cleaner.

## Risks

| Risk | Mitigation |
|------|------------|
| `SpecViolationError` on chunked upload | Fall back to `push_blob` (has its own monolithic fallback). Progress stops but upload succeeds. |
| Upload progress lags by one chunk (16 MiB) | Final callback ensures 100%. For a 100 MB file, this is 16% lag — tolerable. For very small files (<16 MiB), progress jumps from 0 to 100% in one step. |
| `ProgressWriter::poll_write` may be called with partial writes | Track actual bytes returned by inner `poll_write`, not requested bytes. |
| `{bytes}` template variable in indicatif 0.18.x | Verify support. Fallback: use `with_key` for custom formatting. |
| Chunk size mismatch between stream and oci-client | Read chunk size from the same config source. If `NativeTransport` stores `push_chunk_size` from construction, it stays in sync. |

## Notes

- No changes to `external/rust-oci-client/`. All progress tracking lives in `NativeTransport`, `ProgressWriter`, and the `OciTransport` trait.
- The `Publisher` layer doesn't need changes — progress is at the `Client`/transport level.
- The cascade push only copies manifest references — no blob transfer, no progress needed.
- The existing "Resolving" and "Installing" spans in `package_manager/tasks/` remain as outer spinners. The new "Downloading" span nests inside "Installing" via tracing's parent-child relationship, appearing as an indented sub-progress bar.
- `StubTransport` uses the default trait implementations (no progress), so existing tests are unaffected.
- Debug log events follow the same pattern as `archive/tar.rs` (`tracing::debug!` every N items/bytes). They are always compiled in and visible at `-l debug`, regardless of TTY state.
