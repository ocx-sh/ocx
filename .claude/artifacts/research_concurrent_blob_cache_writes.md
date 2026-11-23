# Research: concurrent writers into one content-addressed blob cache directory

## Metadata

**Date:** 2026-11-23
**Lane:** hex-architect research axis (ADR rework `.claude/artifacts/adr_package_export_import.md`)
**Domain:** storage / caching
**Expires:** 2027-05-23
**Basis:** primary sources as linked. The pnpm and cargo fetches failed (404). Git's temp directory and its `core.fsync` default are recalled from memory and marked unverified.

## Question

Several processes, possibly several CI jobs on one machine, fill one opt-in cache directory `blobs/<registry>/<alg>/<shard>/<hex>/data` at the same time. Each blob is written once by streaming: temp file, hash while writing, verify, rename into place. Readers verify on every read. No locks wanted. Eviction and network file systems are out of scope ([ocx-sh/ocx#760](https://github.com/ocx-sh/ocx/issues/760)). What is safe now?

## Findings

1. **Prior art agrees on temp, verify, rename.** Bazel's disk cache, Git loose objects, ccache and Nix write a uniquely named temp file on the same filesystem, verify it, and rename it into place. They tolerate a target that already exists. Bazel documents that concurrent processes can share one disk cache ([DiskCacheClient.java](https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/remote/disk/DiskCacheClient.java)).
2. **Target already exists: drop the temp.** Bazel skips the write and touches the mtime. Git runs `check_collision` and unlinks its temp ([object-file.c](https://raw.githubusercontent.com/git/git/master/object-file.c)). ccache skips the write under `Overwrite::no`.
3. **Locks.** Go ([modfetch/fetch.go](https://raw.githubusercontent.com/golang/go/master/src/cmd/go/internal/modfetch/fetch.go)) and Nix take locks for trees and metadata, never for a single digest-addressed blob. A blob needs no lock, because every writer produces the same bytes.
4. **Windows rename.** Rust's `std::fs::rename` calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`. It falls back to a POSIX-semantics rename only on `ERROR_ACCESS_DENIED` ([windows.rs](https://raw.githubusercontent.com/rust-lang/rust/master/library/std/src/sys/fs/windows.rs)). A reader holding the target open without delete sharing can make the rename fail.
5. **Windows retry.** Go's `robustio` retries a rename on `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION` and `ERROR_FILE_NOT_FOUND`. The budget is 2 s, starting at 1 ms with jittered backoff ([robustio_flaky.go](https://raw.githubusercontent.com/golang/go/master/src/cmd/internal/robustio/robustio_flaky.go), [golang/go#31247](https://github.com/golang/go/issues/31247)).
6. **`tempfile` crate.** `persist` replaces the target. `persist_noclobber` fails if the target exists, and after a crash it can leave a second hard link. Neither syncs, and neither crosses filesystems ([docs](https://docs.rs/tempfile/latest/tempfile/struct.NamedTempFile.html)).
7. **Temp placement.** Put the temp inside the cache root, in a directory readers never scan, with `NamedTempFile::new_in`. A reader then never sees a partial file, and the rename never crosses a device. ccache expires leftover temps after one hour. Nix falls back to a copy on a cross-device error, overlayfs included.
8. **fsync.** Bazel fsyncs the temp before the rename. Git skips fsync for loose objects by default (unverified). Without fsync, a crash can leave a short or zero-length file at the final name. A digest check on read turns that into a miss.

## Recommended write protocol

1. If the final path exists and verifies, stop.
2. Create the temp in `<cache>/tmp/` with an explicit mode, on the same filesystem as `blobs/`.
3. Stream the download into the temp and hash in the same loop.
4. On a digest mismatch, delete the temp and fail.
5. Optionally call `sync_all()` on the temp.
6. Run `create_dir_all` on the parent, then `persist_noclobber`. On `AlreadyExists`, drop the temp. Use `persist` only to replace a blob found corrupt.
7. On Windows, retry the rename for up to 2 s. Count it as success if the target appeared meanwhile and verifies.
8. Readers verify on every read and treat a mismatch as a miss. A later janitor removes `tmp/*` entries older than one hour (#760).

## Not guaranteed

- Durability after power loss.
- Single-fetch deduplication: two jobs may download the same blob.
- Safety against concurrent eviction (#760).
- Network file systems (#760).
- Non-Rust readers on Windows that hold the target past the retry budget.
- Cleanup of orphan temps before a janitor exists.
- Multi-user permissions: umask and group (#760).
