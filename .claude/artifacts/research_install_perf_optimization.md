# Research: `ocx install` Performance Optimization

Date: 2026-06-12. Axes: patterns (industry) + domain (Rust crates) + codebase discovery. Companion: [research_install_bench_tooling.md](./research_install_bench_tooling.md).

## Current Pipeline (discovered, file:line)

Per layer: `oci-client` streams blob → `ProgressWriter` (caps each `poll_write` at 32 KiB, `progress_writer.rs:14-17`) → `tokio::fs::File` (no BufWriter) → **full second read** of file for sha256 (`client.rs:472 verify_blob_digest`) → single-threaded XZ decompress + tar extract in one `spawn_blocking` (`archive.rs:98-128`) → hardlink assembly layers→packages (`assemble.rs`, semaphore 50) → atomic `tokio::fs::rename` into store.

- Packages parallel via `JoinSet` + optional semaphore (`pull.rs:139-178`); layers parallel unbounded (`pull.rs:669`); singleflight dedup package+layer level.
- **oci-client already hashes during stream** (dual digester, `external/rust-oci-client/src/client.rs:1212-1255`) → on-disk re-verify is a redundant second pass.
- Auth token cached per (registry, repo, op), 60 s — no per-blob overhead.
- No download↔extract pipelining: extract starts only after full blob on disk + verified.
- Symlink wiring (Phase 2 of install_all) sequential (`install.rs:61-69`).
- `hardlink::create` calls `create_dir_all(parent)` per file (`hardlink.rs:58-60`) — N+1 no-op mkdirs.
- macOS codesign after extraction (`codesign.rs`); zero cost on Linux.
- No fsync before renames — acceptable for re-downloadable CAS (dpkg/pacman precedent).
- No benches anywhere in workspace (no `[[bench]]`, no criterion/divan).

## Industry Patterns

| Pattern | Who | Verdict for OCX |
|---|---|---|
| Stream→hash→decompress→extract single-pass, no intermediate file in-memory | bun 1.3.13 (libarchive stream, 17× mem reduction), uv (`HashReader` over AsyncRead, hash during extract) | **High value** — exactly what OCX lacks |
| Verify compressed digest during download only; never re-read | containerd content store | **Adopt** — drop second pass |
| xz → zstd payload migration | Arch (+0.8% size, ~14× decompress speedup), Fedora RPM (96 s→7.7 s), OCI `tar+zstd` media type | High value, **separate workstream** (publish-side + media type) |
| Parallel range requests per blob (SOCI/aria2 style) | AWS SOCI (60% faster for 10 GB images) | **Not yet** — OCX blobs 10–200 MB, HTTP/2 multiplexed parallel layers suffice |
| Reflink-first (FICLONE) before hardlink | bun (clonefile/FICLONE/hardlink chain) | Later — ext4 (CI) unsupported; probe+fallback complexity |
| Lazy pull (eStargz/zstd:chunked) | containerd ecosystem | Not now — overkill below 500 MB |
| fsync skip for re-downloadable CAS, rename as atomic publish | pacman vs dpkg debate | Current OCX shape already correct |

## Rust Crate Verdicts (verified 2026-06)

| Item | Crate@ver | Verdict |
|---|---|---|
| Streaming async decompress | `async-compression` 0.4.42 (tokio bufread XzDecoder/ZstdDecoder/GzDecoder; `xz-parallel` feature) | **Adopt** — binstall/uv pattern: `StreamReader` → decoder → `SyncIoBridge` → sync `tar` |
| Hash-while-write | hand-roll `HashingWriter` (~30 lines, `poll_write` tee into `sha2::Digest`); `rattler_digest` 1.3.1 = reference impl, too heavy | **Adopt hand-roll** |
| sha2 hw accel | `sha2` 0.11 — SHA-NI/ARM runtime autodetect default | Already optimal, no config |
| Buffered file writes | `tokio::io::BufWriter::with_capacity(256 KiB–1 MiB)`; tokio File MAX_BUF = 2 MiB since 1.25 | **Adopt** — coalesce the 32 KiB-capped writes; decouple progress callback from write granularity |
| XZ parallel decode | `lzma-rust2` 0.16.3 `XzReaderMt` — exists, but requires `Read + Seek` (file only) + **multi-block** archives. OCX's own `XzWriterMt` (4 MiB blocks) produces multi-block; third-party single-block `.tar.xz` gains nothing. `block_count()` introspection available | Adopt **conditionally** — fallback to single-thread when `block_count() <= 1`. Note write-path unbounded-memory bug issue #83 (watch for mirror) |
| zstd decode | `zstd` (C) via async-compression feature | **Defer** until zstd layers added |
| tar | sync `tar` crate (current) — NOT affected by CVE-2025-62518 (TARmageddon: `tokio-tar`/`async-tar` unpatched/abandoned) | **Keep**; if async tar ever needed → `astral-tokio-tar` 0.5.6 only |
| In-process benches | criterion 0.8.2 (async via `b.to_async`), divan 0.1.21 (no async, has AllocProfiler) | Optional complement to end-to-end harness |

## Prioritized Optimization List

| # | Change | Impact | Complexity |
|---|---|---|---|
| 1 | `BufWriter` (256 KiB) between ProgressWriter and File; remove/keep 32 KiB cap but decouple callback from write size | Medium (≈8× fewer spawn_blocking writes per 50 MB layer) | Low |
| 2 | Drop redundant `verify_blob_digest` second pass — trust oci-client stream hash (containerd model). Verify fork's dual-digester really compares before returning | Medium (eliminates 1 full sequential read per layer) | Low (security reasoning required) |
| 3 | Streaming pipeline: download → hash tee → decompress → tar extract concurrent (single pass). Decision needed: keep compressed blob on disk (tee) vs drop intermediate file (changes retry + CAS semantics) | High (latency hiding I/O↔CPU, biggest win on slow links + big layers) | Medium |
| 4 | Conditional `XzReaderMt` for multi-block OCX-produced archives (post-download path, if 3 not adopted or as fallback) | Medium (CPU-bound xz packages) | Low-Medium |
| 5 | Parallelize symlink Phase 2; cache created dirs in hardlink assembly | Low | Low |
| 6 | zstd layer support (publish + extract) | High but cross-cutting (media type, mirror, backward compat) | Medium — separate feature |

## Risks / Constraints

- **Blob retention**: if `blobs/` CAS keeps compressed blob for GC/refs, single-pass streaming must tee to disk concurrently rather than skip the file. Check `link_blobs` / BlobStore usage before designing option 3.
- Streaming pipeline loses "retry extraction from local blob" on mid-stream failure (re-download instead) — acceptable for benchmark-verified sizes, document trade-off.
- Digest verification change is security-relevant → design record must state the threat model (stream hash = same guarantee; disk re-read only catches local disk corruption).
