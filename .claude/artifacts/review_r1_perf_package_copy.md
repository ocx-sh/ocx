# Review R1 — performance — `ocx package copy`

Reviewer: `reviewer` (opus), focus **performance**. Branch `evelynn`, baseline `main`.
Scope: `crates/ocx_lib/src/oci/copy.rs`, `crates/ocx_lib/src/publisher/copy.rs`,
`crates/ocx_lib/src/oci/client/native_transport.rs`,
`crates/ocx_lib/src/oci/client/transport.rs`, `crates/ocx_cli/src/command/package_copy.rs`.

Every count below is derived by reading the loops and the fork source, not measured.
No timing, throughput or percentage figure appears anywhere in this report (PERF-01).

## Verdict

**Needs work.** The memory claim behind the design holds and is tested. The round-trip
and disk-bound story does not: three redundant network fetches per platform, an
unbounded spool with no size cap, a duplicate full hash pass over every blob, and a
same-path collision when one manifest names a blob digest twice.

## 1. Does the file-backed push actually avoid buffering the blob? — **Yes, verified**

Traced end to end:

- `NativeTransport::push_blob_from_path` (`native_transport.rs:440`) stats the file and
  builds `BlobBody::File { path, size }` — no read.
- `BlobBody::stream` (`native_transport.rs:806`) opens a fresh `tokio::fs::File`, wraps it
  in `ProgressReader`, frames it with `ReaderStream::with_capacity(_, UPLOAD_FRAME_SIZE)`
  (128 KiB) — a lazy stream, nothing materialised.
- `do_push_blob` (`native_transport.rs:730`) hands it to the fork's `push_blob_stream`
  with `Some(total)`. In `external/rust-oci-client/src/client.rs:815-832` the
  `Some(size)` branch wraps the stream in `SharedReader`/`StreamReader` and issues
  `push_chunk_streamed` per `push_chunk_size` window; `push_chunk_streamed`
  (`client.rs:1874`) passes `reqwest::Body::wrap_stream(body)`. Streamed, never collected.
- `push_chunk_size = PUSH_CHUNK_SIZE = 3 MiB` (`builder.rs:28`), so per-request memory is
  bounded by the frame/chunk window, independent of blob size.

`BlobBody::read_all()` (`native_transport.rs:817`) is the one place a File body is
materialised. Its only call site is `do_push_blob:753`, on the `SpecViolationError`
fallback, and it is unreachable above the cap: `do_push_blob:741` returns early when
`total > MAX_UPLOAD_REQUEST_BYTES` (4 MiB, `builder.rs:17`). So a production
`read_all()` on a File body is bounded at 4 MiB. **The stated design intent holds.**

One caveat, not a defect in production: `StubTransport` does **not** override
`push_blob_from_path` (no match in `test_transport.rs`), so every unit test in
`oci/copy.rs` and `publisher/copy.rs` exercises the *default* trait impl
(`transport.rs:208`), which does `tokio::fs::read(path)` — the buffered path. The
streaming path is covered only by the `BlobBody` unit tests and acceptance tests.
`test_transport` is `#[cfg(test)]` (`client.rs:107`), so `NativeTransport` is the only
production transport and the memory claim is not weakened by the default impl.

## 2. Is the retry replay correct? — **Yes, verified and directly tested**

`do_push_blob`'s restart loop calls `body_source.stream(...)` fresh on every attempt
(`native_transport.rs:727`), inside the loop, and each attempt begins a new `POST`
session in the fork. `BlobBody::File::stream` re-opens the file by path, so the second
attempt sends the same bytes from offset 0. `BlobBody::Memory` clones a refcounted
`Bytes`. Pinned by
`native_transport.rs:1398 file_backed_body_streams_the_same_bytes_as_memory_and_replays`,
which drains the same `BlobBody::File` twice and asserts byte equality both times —
that is exactly the truncated-replay failure mode, tested.

## 3. Peak disk — **unbounded, no cap** (finding P-2, P-3)

Shape, read from the loops:

- One `tempfile::tempdir()` per **leaf** (`oci/copy.rs:120`), dropped when `copy_leaf`
  returns, including every error path (`?` unwinds through the local).
- Within a leaf, `copy_blobs` runs at most `MAX_CONCURRENT_BLOB_TRANSFERS = 4`
  transfers concurrently (`oci/copy.rs:231`), each holding one spooled file.
- Platforms are copied **sequentially** in `publisher/copy.rs:185-208`, so leaves do not
  overlap. Referrer blobs reuse the same scratch dir and are copied one referrer at a
  time (`oci/copy.rs:376`), 4-way inside each.
- Success removes the spool immediately (`oci/copy.rs:290`); failure leaves it to the
  `TempDir` drop, which happens on the same return.

So peak disk = the four largest concurrently-transferring blobs of one leaf.

Two problems:

1. **No absolute size cap.** `copy_blob` never checks a declared size. The descriptor
   sizes *are* available — `image.layers[i].size` — and are discarded when
   `blob_digests` is built (`oci/copy.rs:142-146`, digests only). Nothing bounds what a
   source may write to disk before the digest check at `oci/copy.rs:283` fires. This is
   the copy path's analogue of the pull path's caps (`subsystem-oci.md` "Pull Path",
   PKG-05/PKG-07); the copy path has none.
2. **`env::temp_dir()`, not an OCX-owned scratch root.** `tempfile::tempdir()` resolves to
   `$TMPDIR`. On a host where that is a memory-backed filesystem (common in containers
   and on systemd hosts), the spool is RAM and the whole memory argument for
   `push_blob_from_path` is defeated silently; on a host with a small `/tmp` a
   multi-hundred-MB promotion fails on a filesystem the user never chose. The repo
   already owns a scratch facility — `FileStructure`'s `TempStore`
   (`file_structure/temp_store.rs`), rooted under `$OCX_HOME`.

## 4. Concurrency shape

**Sequential at every level except blobs.**

- Blob level: bounded fan-out, `futures::stream::iter(..).map(..).buffer_unordered(4)`
  (`oci/copy.rs:229-233`), cap named `MAX_CONCURRENT_BLOB_TRANSFERS` with a rationale
  comment at `oci/copy.rs:29-34`. **PERF-10 satisfied.**
- **PERF-11 does not apply**: there is no `tokio::spawn` anywhere in the copy path, so
  there is no permit-before-spawn ordering to get wrong. `buffer_unordered` does not
  poll a future until it admits it into the window, and `copy_blob` is an `async fn`, so
  no resource is acquired before admission. This is the correct shape.
- Platform level: sequential (`publisher/copy.rs:185`, `:226`).
- Tag level: sequential (`publisher/copy.rs:235`).
- Referrer level: sequential per subject, recursive with `Box::pin`, bounded by
  `MAX_REFERRER_DEPTH = 8` and `MAX_REFERRERS_PER_LEAF = 256`. Bounded, fine.

Sequential platform/tag copying is a correctness-safe choice and I am not calling it a
defect — but it is worth stating plainly that a 3-platform promotion transfers one
platform's blobs at a time, so the 4-way bound is the *only* parallelism in the command.

One note on `copy_blobs`: it `.collect()`s the whole stream before inspecting any
outcome (`oci/copy.rs:229-238`), so the first error does not cancel the other in-flight
transfers — a copy that is going to fail still finishes uploading up to three more
blobs. Safe (blobs are additive), wasteful.

## 5. Phase-2 sequential merge — **reasoning is right, but it is not the only correct option**

The stated reason (`publisher/copy.rs:222-225`) is sound. `merge_platform_into_index`
(`oci/client.rs:488`) is a genuine read-modify-write: `pull_manifest_raw` → `retain` the
other platforms → `push_manifest_raw` (`client.rs:504`, `:562`, `:581`). Two concurrent
merges of different platforms into the same tag would both read the pre-merge index and
the second write would drop the first platform. Sequential is correct.

But the loop is nested platform-outer, tag-inner, so **each tag is read-modify-written
once per platform**. All platforms for one tag could be merged in a single RMW, cutting
`P × T` index GET+PUT pairs to `T`. The obstacle is real but small: `target_tags` is
platform-dependent (`resolve_cascade_tags` → `has_blocking_platform`,
`cascade.rs:218`, `:305` — a blocker version may offer one platform and not another), so
the batching would have to group platforms by resolved tag set rather than assume one
set. That is a design change, not a mechanical one — see D-1.

## 6. Round-trips for a 3-platform package, cascade writing 4 tags

Derived by reading the loops (cross-registry, no mount, no referrers, no blocker
versions to probe, single tag-list page):

| Phase | Call | Per platform | × 3 |
|---|---|---|---|
| 0 | source index GET + target index GET | — | 2 |
| 1 | leaf manifest GET (`oci/copy.rs:126`) | 1 | 3 |
| 1 | per blob: target HEAD (`:256`) + source blob GET (`:282`) + `blob_exists` HEAD (`native_transport.rs:704`) + POST/PATCH…/PUT | 3+ per blob | — |
| 1 | leaf manifest PUT (`:161`) | 1 | 3 |
| 2 | **`leaf_size` manifest GET** (`publisher/copy.rs:227`) | 1 | **3 — all redundant** |
| 2 | `target_tags` → `list_tags` (`publisher/copy.rs:285`) | ≥1 | **3 — 2 redundant** |
| 2 | merge: 4 tags × (GET + PUT) (`publisher/copy.rs:236`) | 8 | **24 — 16 avoidable** |
| 2 | `push_canonical_tag`: GET + PUT (`oci/client.rs:669`, `:677`) | 2 | 6 |

Three separate fetches of the *same* leaf manifest happen per platform: `copy_leaf`
(`oci/copy.rs:126`), `leaf_size` (`publisher/copy.rs:293`), and `push_canonical_tag`
(`oci/client.rs:669`). Only the first two are in this diff's control;
`push_canonical_tag` is pre-existing shared code and I am not flagging it.

## 7. Blocking work in async (ASYNC-01) — **no violation, one redundant pass**

- No `std::fs`, `Command`, or compression on the new async paths. `oci/copy.rs` uses
  `tokio::fs` throughout.
- `verify_spooled_blob` (`oci/copy.rs:300`) hashes inline in the async task rather than
  under `spawn_blocking`. It hashes per `poll_read` buffer, so the work between awaits
  stays well under the 10–100 µs threshold ASYNC-01 is drawn at. Not a violation.
- It is, however, a **second full read of the spooled file plus a second full sha256
  over the same bytes**, duplicating verification that already happened. The fork's
  `pull_blob` digests every chunk as it writes and compares against the requested digest
  at `external/rust-oci-client/src/client.rs:1452`, `:1471-1477`, raising
  `DigestError::VerificationError`; `registry_error` maps exactly that variant to
  `ClientError::DigestMismatch` (`native_transport.rs:139-142`). So
  `pull_blob_to_file` already fails with the identical error variant, before the push,
  attributing the fault to the source — which is the rationale the doc comment at
  `oci/copy.rs:294-299` gives for the manual pass.

## 8. Deadlines (ASYNC-04) — **no path can hang forever**

Every new network await goes through the one `oci::Client`, built by
`ClientBuilder::new()` with both `read_timeout: Some(REGISTRY_READ_TIMEOUT)` and
`connect_timeout: Some(REGISTRY_CONNECT_TIMEOUT)` (`builder.rs:103-104`), each pinned by
its own assertion (`builder.rs:1126`, `:1138`). The streamed PATCH bodies inherit the
same dispatch-anchored read deadline (`builder.rs:30-38`). The `do_push_blob` restart
loop's worst case is documented in the constant's own doc comment
(`native_transport.rs:633-643`). No finding.

## Findings

### Actionable

- **[High] `crates/ocx_lib/src/oci/copy.rs:142-146` and `:371-376` — duplicate blob
  digests in one manifest collide on the same spool path and run concurrently.**
  `blob_digests` is built without dedup; `copy_blob` derives the spool path from the
  digest hex alone (`:277-278`). A manifest naming the same blob digest twice (legal in
  OCI; identical layers are common in foreign images) puts two `copy_blob` futures in
  the same `buffer_unordered` window, both `truncate(true)`-opening and writing the same
  path (`native_transport.rs:349-355`), then one removing it (`:290`) while the other may
  still need to open it for the push. Nondeterministic `DigestMismatch` or `ENOENT`.
  *Remediation*: dedup before the fan-out — build `blob_digests` through a
  `BTreeSet<Digest>` (or `.unique_clone()` from `VecExt`, already in the prelude) at both
  sites. Also removes the duplicate HEAD/mount round-trips for free.

- **[High] `crates/ocx_lib/src/publisher/copy.rs:227` — `leaf_size` refetches a manifest
  the copy already has, once per platform.** `copy_leaf` returns
  `LeafCopy.size`, documented as "the leaf manifest's size in bytes, for the index
  entry's descriptor" (`oci/copy.rs:85-86`), and `run()` discards it at
  `publisher/copy.rs:204-207` (only `.blobs` and `.referrers` are read). Phase 2 then
  issues a full manifest GET per platform to recompute exactly that number.
  *Remediation*: capture `copied.size` alongside the disposition row in the phase-1 loop
  (a `Vec<(Platform, Digest, i64)>` or a field on `CopiedPlatform`) and read it in phase
  2; delete `leaf_size`. Note phase 2 is skipped under `dry_run`, and phase 1 is the only
  producer of the value, so the two are already gated together — no new branch needed.

- **[High] `crates/ocx_lib/src/oci/copy.rs:282` — the spool is unbounded; the declared
  blob sizes are available and thrown away.** No cap on bytes written to disk before the
  digest is checked; a source that serves an oversized body fills the scratch filesystem
  first. The manifest's own `layer.size` / `config.size` are dropped at `:142-146`.
  *Remediation*: carry `(Digest, i64)` instead of `Digest` into `copy_blobs`, pass the
  declared size to the transfer, and bound the spooled write to it (the pull path's
  `.take(layer.size)` + `ShortBlobRead` shape in `Client::pull_layer` is the in-tree
  precedent). A declared size that is itself absurd should be refused before the fetch.

- **[Warn] `crates/ocx_lib/src/oci/copy.rs:120` — scratch lands in `env::temp_dir()`
  rather than an OCX-owned root.** Defeats the memory argument on a memory-backed
  `$TMPDIR` and puts a multi-hundred-MB spool on a filesystem the operator did not
  choose. *Remediation*: add a scratch-root parameter to `copy_leaf` (and to
  `CopyRequest`), defaulting to `FileStructure`'s `TempStore` root
  (`file_structure/temp_store.rs`) supplied by the CLI at
  `crates/ocx_cli/src/command/package_copy.rs:120`; keep `tempfile::tempdir_in(root)` so
  the sweep-on-drop behaviour is unchanged. The same fix applies to the `--description`
  tempdir at `package_copy.rs:143`.

- **[Warn] `crates/ocx_lib/src/oci/copy.rs:283` + `:300-320` — `verify_spooled_blob` is a
  second full read and second full hash of every transferred blob, duplicating the
  verification `pull_blob_to_file` already performed with the same expected digest and
  the same `ClientError::DigestMismatch` outcome** (evidence in §7 above).
  *Remediation*: drop the second pass and rely on the transport contract — but state that
  contract first: `OciTransport::pull_blob_to_file` (`transport.rs:94`) currently
  documents no verification guarantee, and `StubTransport::pull_blob_to_file` does not
  verify, so the guarantee must be written into the trait doc and honoured by the stub in
  the same change. If the pass is kept deliberately (defence against local corruption
  between write and read), say so in the comment — the rationale currently given is
  already satisfied upstream.

- **[Warn] `crates/ocx_lib/src/publisher/copy.rs:285` — `target_tags` reads the target's
  tag list through `client.list_tags`, which is `ReadAddressing::Mirrored`
  (`oci/client.rs:409-411`), and that listing decides which tags get written.** This is
  `subsystem-oci.md` invariant #5 ("a read that backs a write shares the write's
  addressing"); the merge itself correctly writes canonical
  (`oci/client.rs:499`). *Cross-focus — primarily a security/correctness finding
  (CWE-345/367), reported here because the call site is new in this diff and rev-sec may
  scope it differently.* *Remediation*: `list_tags_addressed(target, ReadAddressing::Canonical)`.
  The mirrored fetches inside `resolve_cascade_tags` (`cascade.rs:313`) are pre-existing
  shared code and out of this diff's scope.

- **[Warn] `crates/ocx_lib/src/oci/copy.rs:286` — a promotion transfers hundreds of MB
  with `no_progress()`.** `push_blob_from_path` takes a `ProgressFn` and the whole
  `ProgressReader` framing exists to drive it; `pull_blob_to_file` reports nothing at
  all. PERF-29 makes an indicator a SHOULD past ~10 s, and `ProgressManager` is already
  the house mechanism (`adr_progress_architecture.md`). *Remediation*: thread the
  `ProgressManager`'s `BytesBar` from the CLI through `CopyRequest` into `copy_blob`, or
  state in the module doc why a copy is deliberately silent.

- **[Suggest] `crates/ocx_lib/src/oci/client/native_transport.rs:730` — `Some(total as usize)`
  narrows `u64` → `usize` with `as`.** PKG-03; lossless on every platform ocx supports
  (all 64-bit per `subsystem-oci.md`), so this is latent, not live. A silent truncation
  would make the fork's `while remaining > 0` loop upload a short blob and fail at the
  committing PUT. *Remediation*: `usize::try_from(total).map_err(|_| ClientError::…)?`.

- **[Suggest] `crates/ocx_lib/src/oci/copy.rs:256` — every uploaded blob is HEADed twice
  at the target**, once by `copy_blob` and once by `do_push_blob`'s `blob_exists`
  (`native_transport.rs:704`). `copy_blob`'s HEAD is load-bearing (it distinguishes
  `Present` from `Uploaded` for the report and gates the mount attempt), so this is one
  extra HEAD per *transferred* blob only. *Remediation*: leave it, or have `copy_blob`
  pass its HEAD result down so the push can skip the re-check. Not worth a signature
  change on its own; fold into the size-plumbing change if that lands.

- **[Suggest] `crates/ocx_lib/src/oci/copy.rs:229-238` — the blob fan-out has no early
  abort.** `.collect()` drains every future before the first `?`. *Remediation*: if a
  hard failure (auth, policy) should stop the remaining transfers, switch to
  `try_for_each_concurrent` or check outcomes as they arrive; if finishing is deliberate
  (partial progress is useful because blobs are additive), say so in a comment.

### Deferred

- **[High] `crates/ocx_lib/src/publisher/copy.rs:226-249` — batching all platforms into
  one index RMW per tag.** For 3 platforms × 4 tags the loop issues 24 index round-trips
  where one merge per tag would issue 8. The blocker is that `target_tags` can legitimately
  differ per platform (`cascade.rs:305-320`), so batching means grouping platforms by
  resolved tag set and extending `merge_platform_into_index` to take a slice of
  `(platform, digest, size)` — a shared helper the push path also uses.
  *Human input needed on*: is the round-trip reduction worth changing a helper that
  `push_with_cascade` depends on, and is a multi-platform merge acceptable as a single
  atomic write (it changes the partial-failure granularity the "most-specific → least
  specific for partial-failure safety" comment at `cascade.rs:249-250` was designed
  around)?

- **[Warn] `crates/ocx_lib/src/publisher/copy.rs:185-208` — platform-level concurrency.**
  Leaves are copied strictly sequentially, so a 3-platform promotion never has more than
  4 blob transfers in flight regardless of available bandwidth. Making it concurrent is
  safe for phase 1 (pure content addition, no shared mutable target state) but each leaf
  would need its own scratch budget, which interacts with the absent disk cap above.
  *Human input needed on*: whether promotion throughput matters enough to spend a
  second bound (leaves × blobs) and a disk budget, or whether sequential is the intended
  conservative default for a production-promotion command.

## Out-of-focus observations (not counted above, for the owning reviewer)

- `publisher/copy.rs:381-384` — `read_target_entries` swallows **every** error from the
  target read, including auth and transient failures, and returns an empty entry list. A
  transient GET failure therefore reports every platform as `Added` and silently drops
  the `KeptNotInSource` rows the function exists to produce. Correctness/spec, not
  performance.
