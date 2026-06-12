# Design Spec: `ocx install` Performance Optimization + Permanent Benchmark Harness

**Status:** Draft  
**Author:** Architect  
**Date:** 2026-06-12  
**Research inputs:**
- `research_install_perf_optimization.md`
- `research_install_bench_tooling.md`

---

## Overview

OCX's install path has four sequential bottlenecks (download → on-disk verify → decompress → extract) where the first blocks all subsequent stages. This spec:

1. Eliminates the redundant on-disk digest re-verify (D2).
2. Replaces the 32 KiB-capped `ProgressWriter`→`File` chain with a `BufWriter`-backed one that decouples progress callback granularity from I/O write size (D3).
3. Establishes a streaming single-pass pipeline (download → hash tee → decompress → extract, D1) that hides I/O latency behind CPU-bound decompression without changing the blob retention contract.
4. Deploys a permanent local benchmark harness (D4) providing before/after regression verification via `task bench`.

All optimizations land in `crates/ocx_lib`; the CLI thin-wrapper pattern is preserved throughout.

---

## Resolved Design Decisions

### D1: Streaming single-pass vs two-pass + smaller fixes

#### Background: blob retention contract (code-confirmed)

The compressed layer blob file (`content.tar.xz` etc.) lives in a `temp/` subdirectory, not in the `blobs/` CAS. `extract_to_temp` in `client.rs:492` wraps the blob path in a `DropFile` guard that deletes it on scope exit. The `blobs/` CAS (`BlobStore`) stores only OCI manifest and config blobs (JSON), never compressed layer tarballs. The `layers/` store holds only extracted content. Therefore: **the compressed blob is temp-only and can be discarded after extraction — no tee to disk is required for CAS preservation**.

#### Option A: Pure streaming single-pass (adopted)

Download stream → `HashingWriter` tee → `async-compression` `XzDecoder` → `SyncIoBridge` → sync `tar` extractor. No blob file written to disk at all. Parallel with download: tar extraction begins as bytes arrive.

**Advantages:**
- Eliminates one full sequential disk-write + full sequential disk-read per layer
- Latency hiding: decompress/extract runs while download is still in flight
- Reduces peak disk I/O (no intermediate compressed file)
- Matches bun 1.3.13 and uv `HashReader` industry pattern exactly

**Disadvantages:**
- On mid-stream failure, re-download is required (cannot resume extraction from partially downloaded blob on disk)
- Requires `async-compression` crate addition (0.4.42, already vetted in research)
- Mid-stream digest mismatch detected late (at stream end), but `oci-client` already detects this at the same point; behavior equivalent

**Trade-off matrix:**

| Criterion | Weight | Option A (streaming) | Option B (two-pass + fixes only) |
|-----------|--------|---------------------|----------------------------------|
| Latency hiding on slow links | High | Excellent — overlap download+extract | None — extract waits for full download |
| Retry cost on failure | Medium | Re-download required | Can retry extract from blob on disk |
| Code complexity delta | Medium | Medium — new async chain | Low — incremental |
| Disk I/O reduction | High | Eliminates compressed blob write+read | None |
| Reversibility | Low | Two-pass can be restored in ~30 min | N/A (baseline) |

**Recommendation: Option A.** Retry-from-blob not currently implemented anyway (pull errors cause full retry regardless), so the re-download trade-off is unchanged from current behavior. The latency and disk-I/O wins are significant. The `async-compression` crate is already budget-approved in research.

**Streaming pipeline contract (review-amended):**

Layering: `OciTransport::pull_blob_streaming` returns the **raw compressed byte stream only** (`impl AsyncRead`). All decompression, hashing, progress, and extraction live in `Client::pull_layer` — the transport trait stays wire-level (SRP; extraction depends on `archive/`, `codesign/`, `utility/` which must not leak into the transport boundary).

Fork API decision: `NativeTransport::pull_blob_streaming` calls the fork's **public `pull_blob_stream`** (`external/rust-oci-client/src/client.rs:1289`) — NOT the private `pull_blob_response`. `pull_blob_stream` wraps the response in `VerifyingStream` (`blob.rs:64-123`), which verifies the digest at stream end and surfaces mismatch as `io::Error(DigestError::VerificationError)`. No fork changes required. The `SizedStream`'s `BoxStream<Result<Bytes, io::Error>>` adapts via `tokio_util::io::StreamReader`.

Two verifiers, one typed error: the fork's `VerifyingStream` (secondary, io::Error) + OCX's `HashingAsyncReader` (canonical, typed). `Client::pull_layer` maps a stream-end `io::Error` whose source is the fork's `DigestError` to `ClientError::DigestMismatch` (never `ClientError::Io`), so the error taxonomy holds regardless of which verifier fires first. `HashingAsyncReader` is retained even though the fork verifies, because: (a) `StubTransport`'s default-impl path has no `VerifyingStream`, (b) the mirror-path invariant (T-A4 replacement) needs an OCX-side verifier independent of transport, (c) SHA-NI makes the second hash ~free.

```
Client::pull_layer:
  transport.pull_blob_streaming(image, digest)   → impl AsyncRead (raw compressed bytes;
  |                                                 NativeTransport: fork pull_blob_stream + StreamReader)
  ├── HashingAsyncReader(sha256)                 → tees compressed bytes into digester
  ├── ProgressReader                             → on_progress(cumulative bytes_read)
  ├── XzDecoder / GzDecoder (async-compression)  → media-type dispatch in pull_layer
  ├── SyncIoBridge (tokio_util::io)              → AsyncRead → sync Read
  └── tar::Archive::unpack()                     → sync extraction into output_dir/content/
      [SyncIoBridge consumed INSIDE spawn_blocking; code comment required explaining why
       this is the correct SyncIoBridge usage (tokio #6795)]

After stream end:
  └── HashingAsyncReader digest vs expected      → ClientError::DigestMismatch on mismatch
      (fork VerifyingStream io::Error with DigestError source also mapped → DigestMismatch)
```

**Where this lands:** `NativeTransport` gains `pull_blob_streaming` (raw stream); the `OciTransport` trait gains the method with a default fallback implementation (download via `pull_blob_to_file`, then stream the file back) so `StubTransport` stays valid unchanged. `Client::pull_layer` assembles the pipeline, absorbing the former `pull_blob_to_file` + `extract_to_temp` sequence at the client layer.

**Blocking-thread occupancy (known trade-off):** with `SyncIoBridge`, the `spawn_blocking` thread is held for the whole download+extract duration of a layer (previously: extract only). At 10 Mbps × 200 MB ≈ 160 s of thread occupancy per layer. Bounded by install/layer parallelism (typical ≤ a few dozen concurrent; Tokio blocking pool cap 512). Documented scale assumption; if install parallelism ever grows unbounded, add a semaphore at the streaming-extract boundary (deferred).

**Invariants preserved:**
- SHA-256 digest verified against the descriptor digest (same guarantee as before, same timing as `oci-client` stream hash)
- `DropFile` guard on blob path removed (no blob file to delete)
- `extract_to_temp` still invoked for codesign step (macOS only) — call site refactored to operate on the already-extracted `content/` directory, not the blob path
- Layer CAS move-to-store (`move_temp_to_object_store`) unchanged — moves `content/` directory as before

---

### D2: Digest verification policy

#### Code-confirmed facts

`oci-client` `pull_blob` (verified at `external/rust-oci-client/src/client.rs:1204-1258`):
1. Maintains a running `layer_digester` updated on every chunk (`line 1229`).
2. Optionally maintains a `header_digester` from the registry's `Docker-Content-Digest` response header (`line 1226`).
3. Calls `out.flush().await?` after the stream ends (`line 1234`) before comparing.
4. Compares the computed digest against the expected layer digest (`line 1249`); returns `DigestError::VerificationError` on mismatch (`line 1250-1254`).

OCX's `verify_blob_digest` (client.rs:55-83) re-reads the entire blob file from disk and hashes it again as a second pass. This is purely redundant.

#### Threat model

The stream hash protects against:
- Registry sending incorrect bytes (content substitution during transit)
- Mid-transfer corruption on the wire

The on-disk re-verify additionally catches:
- Disk corruption occurring between write-complete and verify (race window: milliseconds)
- Silent disk hardware errors on the write path

OCX stores blobs in a content-addressed store (`layers/`) where data is immutable once written. The CAS write path uses `tempfile::NamedTempFile` + atomic rename (`utility::fs::persist_temp_file`). The window between write and verify exists only in `pull_layer`, and the data being verified is temp-before-extraction — never the long-lived CAS content. The re-read adds no protection against CAS-level corruption (that would require a separate integrity check command, which is a separate feature).

Critically: with the streaming pipeline (D1), there is no blob file on disk to re-read. The digest is verified in-stream. The on-disk second pass is eliminated as an inherent consequence of D1.

#### Option A: Drop on-disk re-verify (adopted, mandatory with D1)

With D1, `verify_blob_digest` is never called because no blob file is written. The stream hash provides equivalent integrity over the same data.

**Option B (rejected):** Keep file on disk solely for re-verify, then delete. Adds ~100 ms per 50 MB layer on NVMe, more on spinning disk. Trades real performance for no security uplift, since the write→verify window is sub-millisecond on any storage fast enough to matter.

**One-way-door rationale:** This is not a one-way door. The commit is easily reverted. If a future requirement demands independent blob integrity checking (e.g., for a `verify` subcommand), that is best implemented as an explicit integrity-check task operating on stored layers, not as a side effect of install. The design record captures this intent so the absence of on-disk re-verify cannot be mistaken for an oversight.

**Security classification:** Warn-tier (judgment call), not Block. The stream hash provides the same guarantee as the wire-level verification done by all major container runtimes (containerd, Docker, Podman). The OCX re-read only catches a class of failures (sub-millisecond disk corruption) that no production system treats as a serious threat model for a re-downloadable CAS.

---

### D3: Quick wins set (BufWriter + progress decoupling + parallel symlinks + dir cache)

#### Current state (code-confirmed)

- `ProgressWriter::poll_write` caps each write at `MAX_WRITE_SIZE = 32 KiB` (`progress_writer.rs:17`) to produce frequent progress callbacks. With streaming (D1), `ProgressWriter` is replaced by `ProgressReader` on the read side — the cap and its purpose are re-examined.
- No `BufWriter` between `ProgressWriter` and `tokio::fs::File` — every 32 KiB triggers a syscall.
- Symlink Phase 2 (`install.rs:61-69`): sequential loop over all installed packages.
- `hardlink::create` calls `create_dir_all(parent)` per file (`hardlink.rs:58-60`).

#### Quick wins landing in v1

**QW1: Decouple progress callback from write chunk size (included in streaming pipeline)**
The progress callback should track bytes read from the network stream, not bytes written to disk. With the streaming pipeline, `ProgressReader` wraps the `StreamReader` and calls `on_progress` on each read. The 32 KiB cap on `ProgressWriter` is eliminated — `ProgressReader` calls back on natural chunk boundaries from the network (typically 8–64 KiB HTTP/2 frames). The result: smoother progress without artificial I/O fragmentation.

The `MAX_WRITE_SIZE` constant and existing `ProgressWriter` tests remain valid for use-cases where `ProgressWriter` is still needed (upload path), but the download path no longer uses it.

**QW2: BufWriter 256 KiB for tar extraction output**
The `SyncIoBridge` + `tar` sync extractor writes files to disk via standard `std::fs::File`. A `BufWriter<File>` with 256 KiB capacity coalesces the small tar-entry writes (typically 512-byte headers + variable data) into fewer syscalls. This is a simple wrapping at the `spawn_blocking` boundary.

**QW3: Conditional XzReaderMt (multi-block only)**
`lzma-rust2::XzReaderMt` provides parallel XZ decoding for multi-block streams but requires `Read + Seek` (file input). With D1 (streaming), the input is not seekable. Therefore `XzReaderMt` **cannot apply to the streaming path**.

However, `XzReaderMt` can apply as a post-install optimization: when a layer is being pulled from local cache (already-downloaded compressed blob in a hypothetical future path) or when the streaming path is unavailable. In v1, this is **deferred** — it has no path to apply without a file on disk.

**QW4: Parallel symlink phase**
`install_all` calls `install.rs::create_symlinks` sequentially. This is a simple `JoinSet` dispatch. Impact is low for typical installs (2–10 packages, symlinks are fast). Include as a low-complexity v1 item.

**QW5: Dir cache in hardlink assembly**
`hardlink::create` calls `create_dir_all(parent)` per file. For packages with many files sharing the same directory (common), this is N redundant no-op syscalls. A `HashSet<PathBuf>` of already-created parent directories scoped to the `assemble_from_layer` call eliminates the redundancy.

**Deferred to v2:**
- `XzReaderMt` conditional (no path with streaming D1)
- Reflink-first fallback before hardlink (ext4 unsupported; probe+fallback complexity)
- zstd layer support (separate feature, out of scope)

---

### D4: Benchmark harness architecture

#### Location

`test/bench/` — adjacent to acceptance tests, shares Docker infrastructure, reuses `OcxRunner` and `make_package` helpers. Not inside `test/tests/` (bench is not a pytest test suite). Not a separate top-level `bench/` directory (no reason to separate from existing test infrastructure that it depends on).

**Directory layout:**
```
test/bench/
  __init__.py
  conftest.py          # bench-specific fixtures (toxiproxy client, bench registry)
  harness.py           # BenchRunner, ScenarioResult, parallel_install_wall_clock()
  scenarios.py         # scenario definitions (bandwidth profiles, package sizes)
  baseline.py          # curl+tar floor baseline command builder
  compare.py           # threshold comparison: load JSON baseline, compare current, emit report
  results/             # .gitignore'd; per-run JSON outputs land here
```

#### hyperfine acquisition

hyperfine is not currently mirrored on ocx.sh (confirmed: no mirror exists for it in current `ocx.toml`). Acquisition options:

**Option A: Add to `ocx.toml` as dogfood candidate** — requires publishing hyperfine to ocx.sh registry first. Blocked until ocx.sh registry has hyperfine published.

**Option B: `gh release download` in taskfile** — `gh release download -R sharkdp/hyperfine --pattern 'hyperfine-*-x86_64-unknown-linux-musl.tar.gz'`. Requires `gh` CLI present (available in CI via GitHub Actions runner).

**Option C: System package manager** — `apt-get install -y hyperfine` in CI; user installs locally. Platform-inconsistent version.

**Recommendation: Option B for now, with a `status:` check that skips download if `hyperfine` already on PATH.** Add a comment in the taskfile to revisit once hyperfine is mirrored on ocx.sh (dogfood opportunity). This is boring, deterministic, and does not block the bench harness on publishing hyperfine.

#### toxiproxy as docker-compose service

Added to `test/docker-compose.yml` under a `bench` profile so `docker compose up -d` for the standard test run is unaffected:

```yaml
services:
  bench-proxy:
    image: ghcr.io/shopify/toxiproxy:2.12.0
    ports:
      - "5002:5002"   # proxied registry endpoint
      - "8474:8474"   # toxiproxy REST API
    profiles:
      - bench
```

toxiproxy is initialized by `bench/conftest.py` at bench session start: it creates a proxy from `:5002` → `registry:5000` (service-to-service via Docker network) using the REST API on `:8474`. Bandwidth toxics are applied and removed per scenario.

No profile means `task test` never starts the proxy container. `task bench` brings up the `bench` profile explicitly.

#### Scenario matrix

| Scenario | Bandwidth | Latency | Package size | Concurrency | Shape | Cold/Warm |
|----------|-----------|---------|--------------|-------------|-------|-----------|
| `baseline_curl_10mbps` | 10 Mbps | 0 ms | 50 MB | 1 | floor (curl+tar) | cold |
| `baseline_curl_100mbps` | 100 Mbps | 0 ms | 50 MB | 1 | floor (curl+tar) | cold |
| `ocx_install_10mbps` | 10 Mbps | 0 ms | 50 MB | 1 | single | cold |
| `ocx_install_100mbps` | 100 Mbps | 0 ms | 50 MB | 1 | single | cold |
| `ocx_install_high_bandwidth` | 1 Gbps nominal¹ | 0 ms | 50 MB | 1 | single | cold² |
| `ocx_install_unthrottled` | unthrottled | 0 ms | 50 MB | 1 | single | cold² |
| `ocx_install_latency_50ms` | 100 Mbps | 50 ms | 50 MB | 1 | single | cold |
| `ocx_install_small_10mbps` | 10 Mbps | 0 ms | 5 MB | 1 | single | cold |
| `ocx_install_large_10mbps` | 10 Mbps | 0 ms | 200 MB | 1 | single | cold |
| `ocx_parallel_2_100mbps` | 100 Mbps | 0 ms | 50 MB | 2 | Shape 1: one `ocx install a b` | cold |
| `ocx_parallel_4_100mbps` | 100 Mbps | 0 ms | 50 MB | 4 | Shape 1: one `ocx install a b c d` | cold |
| `ocx_parallel_2_processes` | 100 Mbps | 0 ms | 50 MB | 2 | Shape 2: 2 concurrent processes | cold |
| `ocx_parallel_4_processes` | 100 Mbps | 0 ms | 50 MB | 4 | Shape 2: 4 concurrent processes | cold |
| `ocx_install_warm` | unthrottled | 0 ms | 50 MB | 1 | single | warm (index populated) |

"Cold" = fresh `OCX_HOME`, no local index, no cached layers. "Warm" = index pre-populated, no cached layers (tests network bottleneck vs. index-lookup overhead).

¹ "1 Gbps" is nominal: Docker veth/WSL2 virtual NIC may cap below the toxic rate — the scenario measures behavior at the environment's upper bandwidth limit, hence the name `high_bandwidth`.
² Cold refers to OCX state only ("warm-FS, cold-OCX_HOME"): the Linux page cache is NOT dropped between runs (requires CAP_SYS_ADMIN/sudo — not portable). For throttled scenarios the network dominates and page cache is irrelevant; for `unthrottled`/`high_bandwidth` the numbers are warm-FS by definition. Documented in `test/bench/README.md`.

#### Baseline floor command

The baseline is `curl <blob-url> | tar -xJ -C <tmpdir>` against the same locally-hosted registry:2 via the toxiproxy endpoint. This isolates raw HTTP-download-plus-extract overhead as the theoretical floor. hyperfine compares named commands natively so the ratio appears in the JSON export.

The `curl` command: `curl -sS http://localhost:5002/v2/<repo>/blobs/<digest> | tar -xJ -C "$tmpdir"`. The blob URL is constructed by `baseline.py` from the `PackageInfo` fixture. The blob is the layer blob (compressed tarball), not the manifest.

#### Parallel-install measurement

Two shapes, both measured:

**Shape 1: Single `ocx install a b c d` invocation.** This exercises the existing `install_all` JoinSet parallel dispatch. hyperfine times the invocation directly.

**Shape 2: N concurrent `ocx install <pkg>` processes.** Because hyperfine cannot time concurrent subprocesses, `harness.py::parallel_install_wall_clock(n, pkg)` uses `asyncio.TaskGroup` to fire N `ocx install` subprocesses concurrently and measures wall-clock time from first launch to last completion. Output is emitted as hyperfine-compatible JSON for unified pipeline.

#### JSON output and threshold comparison

hyperfine `--export-json bench-<scenario>.json` per scenario. After all scenarios complete, `compare.py` loads a `baseline.json` (committed or saved from a reference run) and the current results, computes ratios, and emits a pass/fail report:

```
scenario                    baseline_mean  current_mean  ratio  status
ocx_install_100mbps         12.3s          8.1s          0.66x  PASS  (threshold 0.85x)
ocx_install_10mbps          45.2s          29.8s         0.66x  PASS
ocx_parallel_4_100mbps      15.1s          9.2s          0.61x  PASS
...
```

Threshold: 0.85x (current must be no slower than 85% of baseline). Regressions emit non-zero exit. The threshold is conservative — the optimizations should deliver 40–60% improvement on network-bound scenarios.

#### Before/after protocol

1. On main (before optimizations), run `task bench:baseline` — saves results to `test/bench/baseline.json` (committed to repo as the reference).
2. After implementing optimizations, run `task bench` — compares against `baseline.json`.
3. `baseline.json` is updated when intentionally changing the reference point (new hardware, changed scenario matrix, confirmed regression-is-acceptable tradeoff).

#### taskfile targets

New `bench:*` namespace in `test/taskfile.yml`:

```yaml
bench:setup:
  desc: Start bench docker-compose profile (registry + toxiproxy)
  cmds:
    - docker compose -f {{.TASKFILE_DIR}}/docker-compose.yml --profile bench up -d

bench:teardown:
  desc: Stop bench docker-compose profile
  cmds:
    - docker compose -f {{.TASKFILE_DIR}}/docker-compose.yml --profile bench down

bench:baseline:
  desc: Run benchmark and save as baseline
  deps: [build, bench:setup, .bench:acquire-hyperfine]
  cmds:
    - uv run python bench/harness.py --save-baseline
  env:
    OCX_COMMAND: "{{.OCX_BINARY}}"
    BENCH_RESULTS_DIR: "{{.TASKFILE_DIR}}/bench/results"

bench:
  desc: Run benchmark and compare against baseline
  deps: [build, bench:setup, .bench:acquire-hyperfine]
  cmds:
    - uv run python bench/harness.py
    - uv run python bench/compare.py bench/baseline.json bench/results/latest.json
  env:
    OCX_COMMAND: "{{.OCX_BINARY}}"
    BENCH_RESULTS_DIR: "{{.TASKFILE_DIR}}/bench/results"

.bench:acquire-hyperfine:
  internal: true
  status:
    - which hyperfine
  cmds:
    - gh release download -R sharkdp/hyperfine --pattern 'hyperfine-*-x86_64-unknown-linux-musl.tar.gz' -D /tmp/hyperfine-dl
    - tar -xzf /tmp/hyperfine-dl/hyperfine-*.tar.gz -C /tmp/hyperfine-dl
    - install -m755 /tmp/hyperfine-dl/hyperfine-*/hyperfine ~/.local/bin/hyperfine
```

---

## Component Contracts

### `HashingAsyncReader<R: AsyncRead>`

**Location:** `crates/ocx_lib/src/oci/client/hashing_reader.rs` (new)

**Contract:**
```rust
/// An AsyncRead wrapper that computes a running SHA-256 digest over all bytes read.
pub(crate) struct HashingAsyncReader<R> {
    inner: R,
    digester: sha2::Sha256,
    bytes_read: u64,
}

impl<R: AsyncRead + Unpin> HashingAsyncReader<R> {
    pub fn new(inner: R) -> Self;
    /// Returns the digest over all bytes successfully read so far, plus byte count.
    /// Consumes self.
    #[must_use]
    pub fn finalize(self) -> (oci::Digest, u64);
}

impl<R: AsyncRead + Unpin> AsyncRead for HashingAsyncReader<R>;
```

**Semantics (testable):** `finalize()` is callable at any point and deterministically returns the digest of exactly the bytes that successful reads returned. The *caller protocol* in `pull_layer` is: on any mid-stream I/O error, abort the pipeline and propagate the error — never call `finalize()` for verification after an error (a partial digest would simply mismatch, which is also safe, but the I/O error is the primary failure). Only after EOF is the finalize digest compared against the descriptor digest.

**Error behavior:** I/O errors from inner `AsyncRead` are propagated unchanged. Bytes from errored reads are never fed to the digester (only successfully-filled buffer slices update it).

---

### `ProgressReader<R: AsyncRead>`

**Location:** `crates/ocx_lib/src/oci/client/progress_reader.rs` (new, replaces `ProgressWriter` on download path)

**Contract:**
```rust
pub(crate) struct ProgressReader<R> {
    inner: R,
    on_progress: ProgressFn,
    bytes_read: u64,
    total: u64,
}

impl<R: AsyncRead + Unpin> ProgressReader<R> {
    pub fn new(inner: R, total: u64, on_progress: ProgressFn) -> Self;
}

impl<R: AsyncRead + Unpin> AsyncRead for ProgressReader<R>;
```

**Invariant:** `on_progress(bytes_read_so_far)` called after each successful read. Callback is non-blocking. Callback is called with cumulative total, same contract as existing `ProgressWriter`.

`ProgressWriter` is **not removed** — it remains for the upload path (`push_package`). Only the download path switches to `ProgressReader`.

---

### `OciTransport::pull_blob_streaming` (new trait method)

**Location:** `crates/ocx_lib/src/oci/client/transport.rs`

```rust
/// Type alias matching the existing progress callback in progress_writer.rs.
pub(crate) type ProgressFn = std::sync::Arc<dyn Fn(u64) + Send + Sync>;

/// Streams the RAW (compressed) blob bytes from the registry.
///
/// Returns an `AsyncRead` over the compressed bytes exactly as served.
/// NO decompression, NO hashing, NO progress — those are assembled by the
/// caller (`Client::pull_layer`). The transport stays wire-level.
///
/// NativeTransport impl: fork `pull_blob_stream` (public; VerifyingStream
/// included — digest mismatch surfaces as io::Error with DigestError source
/// at stream end) adapted via `tokio_util::io::StreamReader`.
///
/// # Errors (from the returned reader)
/// - io::Error (BlobNotFound at call time → ClientError::BlobNotFound)
/// - io::Error with fork DigestError source at stream end
///   (caller maps → ClientError::DigestMismatch)
async fn pull_blob_streaming(
    &self,
    image: &oci::native::Reference,
    digest: &oci::Digest,
) -> Result<Box<dyn AsyncRead + Send + Unpin + 'static>>;
```

(`total_size` / `on_progress` removed from the signature — progress is the caller's `ProgressReader` concern, not the transport's.)

**Default implementation:** provided on `OciTransport`, delegates to `pull_blob_to_file` into a temp file + streams the written file back as `AsyncRead`. This keeps `StubTransport` in unit tests compilable without change. Note: the default path has no `VerifyingStream` — `HashingAsyncReader` in `pull_layer` is the sole verifier there (which is exactly why it is canonical).

---

### `Client::pull_layer` (modified)

**Pre-condition:** layer descriptor must have a supported compression media type (`CompressionAlgorithm::from_media_type` succeeds).

**Post-condition:** `output_dir/content/` exists and contains the extracted layer contents. No blob file (`.tar.xz` etc.) exists in `output_dir` after return.

**Digest verification:** SHA-256 of the compressed stream matches `layer.digest` before extraction begins (stream hash). If mismatch: returns `ClientError::DigestMismatch`, `output_dir` is in an indeterminate state (caller must not move to CAS).

**Error cases:**
| Error | Cause | Recovery |
|-------|-------|----------|
| `DigestMismatch` | Stream hash ≠ descriptor digest | Full re-download (same behavior as before) |
| `Io { path, .. }` | Disk write failure during extraction | Caller's temp cleanup (DropFile equivalent) |
| `InvalidManifest` | Unsupported media type | Fix publisher; package is malformed |
| `Internal` | tar extraction error, codesign failure | Log + retry |

---

### `BenchRunner`

**Location:** `test/bench/harness.py`

```python
@dataclass
class ScenarioResult:
    scenario_name: str
    mean_seconds: float
    stddev_seconds: float
    min_seconds: float
    max_seconds: float
    runs: int
    command: str

class BenchRunner:
    def __init__(self, ocx_binary: Path, registry_url: str, toxiproxy_api_url: str): ...

    def run_scenario(
        self,
        scenario: Scenario,
        warmup: int = 1,
        runs: int = 10,  # hyperfine --min-runs 10; 5 runs gives too-wide stddev for the 0.85x gate
    ) -> ScenarioResult:
        """Apply toxiproxy config, run hyperfine, return parsed result."""

    def parallel_install_wall_clock(
        self,
        packages: list[str],
        env: dict[str, str],
        runs: int = 10,
    ) -> ScenarioResult:
        """Fire len(packages) ocx install subprocesses concurrently via TaskGroup,
        measure wall-clock time from first launch to last completion.
        Shape 2 measurement; Shape 1 (single multi-package invocation) goes
        through run_scenario/hyperfine directly."""

    def save_results(self, results: list[ScenarioResult], path: Path) -> None:
        """Write hyperfine-compatible JSON with all scenario results."""

    def compare_against_baseline(
        self,
        baseline: Path,
        current: list[ScenarioResult],
        threshold: float = 0.85,
    ) -> CompareReport:
        """PURE: returns the report only — never exits. The __main__ block of
        compare.py calls this and translates report.failed into a non-zero
        process exit. Keeps the method unit-testable."""
```

**Invariant:** `run_scenario` always tears down the toxiproxy toxic after the run, even on failure. Registry and toxiproxy are session-scoped, not per-scenario.

**Entry-point model (resolves conftest/harness split):** the bench is **standalone-script-driven** — `harness.py` is the entry point invoked by the taskfile (`uv run python bench/harness.py`); it owns session lifecycle (toxiproxy proxy creation, reachability assert, teardown). `conftest.py` exists only for the pytest-based smoke validation test of the harness itself and reuses the same helpers. `BenchRunner` owns per-scenario toxic state.

---

## User Experience Scenarios

### Scenario 1: Single-package install on slow link (10 Mbps)

**Before:** `ocx install cmake:3.28` takes 45 s (download 40 s + re-verify 3 s + extract 2 s sequential).
**After:** 30 s (download+extract overlap; no re-verify; time dominated by network).

Action: `ocx install cmake:3.28`
Outcome: Package installed at `~/.ocx/layers/...` and `~/.ocx/packages/...`. Progress bar advances smoothly as bytes arrive. On completion, candidate symlink created as before.
Error case: If network is interrupted mid-stream, `ClientError::Io` is returned. The partial temp directory is cleaned up by the existing `TempStore` cleanup path (unchanged). Retry behavior is identical to current.

### Scenario 2: Parallel install of 4 packages

**Before:** 4 × sequential (download → verify → extract) with JoinSet parallelism. If all packages are 50 MB on 100 Mbps, wall clock ≈ max(each package time) ≈ 15 s.
**After:** Each package streams download+extract concurrently; no re-verify. Wall clock ≈ 9–10 s.

Action: `ocx install cmake:3.28 ninja:1.11 ccache:4.9 clang:18`
Outcome: All 4 packages installed. Progress spinners for each run concurrently. Order of completion non-deterministic; results sorted by input order before return (existing `_all` method contract preserved).

### Scenario 3: Benchmark run before optimization

Action: `task bench:baseline`
Outcome: Builds binary, starts bench Docker profile, creates 5/50/200 MB packages via `make_package`, runs all scenarios against the 2-pass implementation, saves `test/bench/baseline.json`.
Error case: Docker not running → task fails with clear docker-compose error. toxiproxy not reachable → `bench:setup` fails before scenarios run.

### Scenario 4: Benchmark run after optimization

Action: `task bench`
Outcome: Runs all scenarios against the streaming implementation, compares against `baseline.json`, prints tabular report with ratios. Exits 0 if all scenarios pass threshold (≤ 0.85x regression). Exits 1 if any scenario regresses. CI can gate on this.

### Scenario 5: Mid-stream digest mismatch (adversarial registry)

Action: Registry serves bytes that produce incorrect SHA-256.
Outcome: `HashingAsyncReader::finalize()` returns mismatched digest; `pull_layer` returns `ClientError::DigestMismatch`. `pull_all` converts to `PackageErrorKind::Internal`. CLI prints error with identifier context and exits with `ExitCode::DataError` (65). Temp directory cleaned up.

---

## Error Taxonomy

No new error variants are added. Existing variants cover all new code paths:

| Error | Existing variant | Notes |
|-------|-----------------|-------|
| Stream digest mismatch | `ClientError::DigestMismatch` | Replaces the `verify_blob_digest` call path |
| I/O during streaming | `ClientError::Io { path, source }` | Extraction target path |
| Unsupported media type | `ClientError::InvalidManifest` | Unchanged |
| tar extraction failure | `ClientError::Internal` | `archive::Error` wrapped |
| codesign failure (macOS) | `ClientError::Internal` | Unchanged |

---

## Edge Cases

### Edge case 1: Partially-extracted layer on process kill

If the process is killed mid-extraction, the temp directory survives. The existing `TempStore` GC in `clean` handles orphaned temp directories. No change required.

### Edge case 2: Layer already in CAS (singleflight dedup)

`extract_layer_atomic` uses the `LayerGroup` singleflight gate. A second concurrent pull of the same digest waits for the first to complete, then finds it in the CAS via `find_plain`. The streaming pipeline runs only for the first caller; the second caller skips download+extract entirely. This interaction is unchanged.

### Edge case 3: Download stream returns empty body

`StreamReader` over an empty stream completes immediately. `HashingAsyncReader::finalize()` returns the SHA-256 of zero bytes. This will not match any valid layer digest (`sha256:e3b0c...` is not a valid layer), so `DigestMismatch` is returned. This case previously resulted in an empty blob file on disk triggering the same error in `verify_blob_digest`. Behavior is equivalent.

### Edge case 4: XZ stream that is not a valid tar archive

The `spawn_blocking` tar extractor returns an error. This surfaces as `ClientError::Internal`. The temp directory is in a partially-extracted state. `DropFile` equivalent cleanup (current: DropFile on blob path, new: TempStore cleanup on output_dir) handles this. Unchanged semantics.

### Edge case 5: toxiproxy REST API unreachable during bench setup

`bench/conftest.py` asserts toxiproxy is reachable before running any scenarios. If unreachable, the session fails with a clear error before any scenarios run. No partial benchmark results are saved.

### Edge case 6a: Backpressure across the async→sync boundary

No explicit backpressure cap exists between the network stream and the tar extractor beyond TCP receive buffers and the decoder's internal buffer. If decompression were slower than delivery, decoded data would accumulate in `XzDecoder`'s output buffer — bounded in practice (XZ decode ≫ network rates in scope; pre-streaming design buffered the entire compressed blob on disk, so the new pipeline is strictly better). Known non-issue at current blob sizes; revisit only if blob sizes grow ≥1 GB.

### Edge case 6: hyperfine not on PATH and `gh` CLI absent

The `.bench:acquire-hyperfine` task fails with a clear error at the `gh release download` step. The user is directed to install `gh` CLI or install `hyperfine` manually. `status: which hyperfine` ensures this step is skipped if hyperfine is already installed.

---

## Trade-off Summary Table

| Decision | Adopted option | Key trade-off accepted |
|----------|---------------|----------------------|
| D1: Pipeline | Streaming single-pass | Re-download on failure (not a regression; already required) |
| D2: Re-verify | Drop on-disk re-verify | No protection against sub-millisecond disk corruption (accepted per threat model) |
| D3: Quick wins | BufWriter + ProgressReader + parallel symlinks + dir cache; defer XzReaderMt | XzReaderMt incompatible with streaming (no seekable file); deferred cleanly |
| D4: Harness | test/bench/ + hyperfine via gh release | Requires Docker + gh CLI for bench (not for regular test/verify) |

---

## Implementation Phasing

### Phase 1: Quick wins (low risk, standalone)

1. Add `BufWriter<File>` at tar extraction write sites.
2. Parallel symlink phase in `install_all`.
3. Dir cache in `hardlink::create` / `assemble_from_layer`.

(`ProgressReader` is NOT a Phase 1 item — it wraps the `StreamReader` that only exists once the streaming pipeline lands. It ships with Phase 2; spec D3 QW1 and the plan agree on this.)

Gate: `task rust:verify` passes. These are pure refactors with no semantic change.

### Phase 2: Streaming pipeline (medium risk)

1. Add `HashingAsyncReader` + `ProgressReader` structs.
2. Add `pull_blob_streaming` to `OciTransport` trait with default fallback (raw compressed stream).
3. Implement `pull_blob_streaming` on `NativeTransport` via fork `pull_blob_stream`.
4. LSP audit of `extract_to_temp` callers (findReferences) before refactor; then refactor `Client::pull_layer` to assemble the streaming pipeline; codesign step operates on extracted `content/`.
5. Remove `verify_blob_digest` call + blob-path `DropFile`; add fork-DigestError → `ClientError::DigestMismatch` mapping.
6. **Test replacement (explicit):** delete `verify_blob_digest_*` unit tests (client.rs:1222-1295) and T-A4 (`pull_layer_rejects_tampered_blob_under_configured_mirror`, client.rs:1351) — replace with streaming equivalents: (a) tampered stream → `ClientError::DigestMismatch` (not `Io`) on both Native and Stub/default-impl paths, (b) mirror-path invariant restated: host+repo rewrite cannot bypass `HashingAsyncReader` verification, (c) mid-stream I/O interruption → `ClientError::Io` + temp dir cleaned by TempStore path.

Gate: `task rust:verify` passes. Existing `pull_layer` acceptance tests (test_install.py) pass. CWE-345 protection demonstrably re-tested (no coverage regression).

### Phase 3: Benchmark harness

1. Add `bench-proxy` service with profile to `test/docker-compose.yml`.
2. Create `test/bench/` module.
3. Add `bench:*` tasks to `test/taskfile.yml`.
4. Run `task bench:baseline` on main (before Phase 2 lands) to establish baseline.
5. After Phase 2 lands, run `task bench` to verify improvement.

Gate: `task bench` exits 0 with measured ratios ≤ 0.85x for all scenarios.

---

## New Dependencies

| Crate | Version | Scope | Justification |
|-------|---------|-------|---------------|
| `async-compression` | 0.4.42 | `ocx_lib` | Async XZ decode without intermediate file. No alternative in existing deps. Vetted in research. |
| `tokio-util` | latest stable | `ocx_lib` | `SyncIoBridge` (`io-util` feature). **New direct dep** — confirmed absent from root + ocx_lib Cargo.toml (grep 2026-06-12, 0 matches); present transitively via tokio ecosystem. |
| `liblzma` (transitive) | via `compression-codecs` | audit only | async-compression ≥0.4.38 routes xz through `compression-codecs` → `liblzma` (NOT `xz2`). CVE-2025-31115 affects `lzma_stream_decoder_mt` (multithreaded decode) in bundled XZ 5.3.3α–5.8.0 — OCX uses the single-threaded `XzDecoder` path, not exploitable, but `cargo audit`/`deny` may flag: verify bundled XZ ≥5.8.1 or document the exception in `deny.toml` with this rationale. New transitive deps go through the `deps` skill checklist (license + deny). |

No new Python dependencies for the bench harness — uses existing `uv`-managed venv, `asyncio.TaskGroup` (stdlib), and hyperfine as an external binary (not a Python package).

---

## Files Affected

### New files
- `crates/ocx_lib/src/oci/client/hashing_reader.rs`
- `crates/ocx_lib/src/oci/client/progress_reader.rs`
- `test/bench/__init__.py`
- `test/bench/conftest.py`
- `test/bench/harness.py`
- `test/bench/scenarios.py`
- `test/bench/baseline.py`
- `test/bench/compare.py`
- `test/bench/baseline.json` (committed after first baseline run)
- `test/bench/results/` (gitignored)

### Modified files
- `crates/ocx_lib/src/oci/client/transport.rs` — add `pull_blob_streaming` method with default
- `crates/ocx_lib/src/oci/client/native_transport.rs` — implement `pull_blob_streaming`
- `crates/ocx_lib/src/oci/client.rs` — remove `verify_blob_digest` call; refactor `pull_layer` and `extract_to_temp`
- `crates/ocx_lib/src/oci/client/progress_writer.rs` — add note that this file covers upload path; download path now uses `progress_reader.rs`
- `crates/ocx_lib/src/file_structure/hardlink.rs` — dir cache in `create`
- `crates/ocx_lib/src/package_manager/tasks/install.rs` — parallel symlink phase
- `test/docker-compose.yml` — add `bench-proxy` service with `bench` profile
- `test/taskfile.yml` — add `bench:*` targets
- `Cargo.toml` (ocx_lib) — add `async-compression`

### Not affected
- `crates/ocx_cli/` — no CLI changes required; lib orchestration hosts all changes
- `crates/ocx_lib/src/file_structure/blob_store.rs` — BlobStore contract unchanged
- `crates/ocx_lib/src/package_manager/tasks/pull.rs` — extract_layers / setup_impl unchanged; changes are in the lower-level `client.rs` layer
- `external/rust-oci-client/` — no fork changes required; D2 removes the OCX re-verify without touching the fork's in-stream verify

---

## Open Questions for Human Review

None of these are blockers; they are judgment calls for the human to confirm before implementation begins.

1. **tokio-util in tree?** Confirm `tokio-util` is already a dependency of `ocx_lib` before Phase 2 starts. If not, it is a trivial add (same Tokio ecosystem, well-established).

2. **bench baseline.json commit strategy.** Should `baseline.json` be committed to the main branch (so CI can compare) or kept developer-local only (compare.py only runs locally)? Committing it means the baseline drifts as hardware changes. Keeping it local means CI cannot gate on regressions. Recommendation: commit baseline to repo, document that it represents "reference hardware" (the developer machine where first baseline was run), and accept that absolute numbers vary across machines while relative ratios (within a machine) are stable.

3. **CI integration of bench target.** The spec defers CI integration as optional. Confirm this is acceptable — the bench harness is permanent and locally runnable but not wired into the CI gate for now. If CI integration is desired, the main constraint is Docker availability on the runner and the ~5–10 min runtime for the full scenario matrix.

4. **`ProgressWriter` removal from download path.** The `progress_writer.rs` file and its tests remain (upload path still needs it). Confirm there is no concern about having two "write-progress" abstractions in the codebase simultaneously (`ProgressWriter` for upload, `ProgressReader` for download). The names are deliberately asymmetric to match their respective I/O directions.

5. **XzReaderMt future path.** The conditional `XzReaderMt` quick win is deferred because it requires a seekable file input, which the streaming pipeline does not provide. If a future requirement adds a "verify stored layer integrity" command that re-reads stored layers from disk, `XzReaderMt` would be the natural acceleration path there. Confirm this deferral is acceptable.
