# Research: Streaming zip extraction in a security-sensitive OCI layer pipeline

**Axis 2/3** for ocx-sh/ocx#183. Produced 2026-08-09 during `/hex-plan`
Discover+Research.

## Direct answer

**Buffer-to-disk → digest-verify → then parse via `Seek`** is the only defensible
shape for an untrusted zip layer. True single-pass streaming extraction (mirroring
the tar path) is technically possible via local-file-header readers, but every
crate surveyed documents it as strictly less safe, and it reopens the exact
vulnerability class zip's dual-header design created.

`rc-zip`'s own README states the crux: *"Only the central directory is
authoritative when it comes to the contents of a zip archive."*

## Key findings

1. **`zip` crate** (vendored here at 8.6.0; maintained fork
   [zip-rs/zip2](https://github.com/zip-rs/zip2)) exposes
   `read_zipfile_from_stream` for non-seekable input, with docs stating: *"If
   possible, use the `ZipArchive` functions as some information will be missing."*
   Missing when streaming: `comment`, `data_start`, and
   `external_attributes`/`unix_mode()` → `None`. Given research axis 3's finding
   that the exec bit is already the fragile part, losing `unix_mode()` entirely is
   disqualifying on its own.

2. **`async_zip`** ([Majored/rs-async-zip](https://github.com/Majored/rs-async-zip))
   documents the security-relevant gaps more explicitly: with a data descriptor,
   **CRC, compressed size, and uncompressed size are all unavailable**, and *"the
   extra field data potentially being inconsistent with what's stored in the
   central directory."* Data descriptors are common in CI-produced (streamed-write)
   zips.

3. **`rc-zip`** ([bearcove/rc-zip](https://github.com/bearcove/rc-zip)), a sans-io
   state-machine parser, warns that repacked archives *"may contain duplicate local
   file headers (and data), along with headers for entries that have been
   removed"* — a zip-format library author independently confirming the question.

4. **Attack precedent — CVE-2013-4787 ("Android Master Key")**
   ([NVD](https://nvd.nist.gov/vuln/detail/CVE-2013-4787)). Exploited exactly this
   class: Android's installer and runtime picked *different* entries among
   duplicate-named zip entries, so the signature check validated one entry while
   the OS installed another — malicious code injected into a signed APK without
   invalidating the signature.

4b. **CVE-2025-54368 (uv, August 2025) — the closest possible analogue, and the
   reason digest verification alone is NOT sufficient.**
   [astral.sh advisory](https://astral.sh/blog/uv-security-advisory-cve-2025-54368).
   A Rust-based Python package installer — same language, same problem domain,
   same year. Two exploited differentials:
   - **Dangling local entries** with no central-directory header. The spec
     considers this invalid, *"but streaming parsers typically ignore the
     requirement."*
   - **"Doubled ZIP"** — the central-directory offset is spec-ambiguous (absolute
     vs relative to EOCDR), so uv's parser and Python's `zipfile` disagreed about
     *which* central directory to read.

   An attacker crafts **one archive, with one digest throughout**, that installs
   different content depending on which parser reads it. uv's fix went beyond
   verify-then-parse: fully consume the central directory, **reject any
   local-entry/central-directory mismatch**, reject bad CRC/size, and reject
   suspicious EOCDR comment fields.

   **Planning consequence: a digest match does not prove parser agreement.**
   Verify-then-parse is necessary but insufficient on its own.

5. **Filename spoofing**: a parser reading the filename from the local header for
   one purpose and the central directory for another lets an attacker present one
   identity to a scanner and another to the extractor
   ([Ostorlab](https://blog.ostorlab.co/zip-packages-exploitation.html)).

6. **Overlapping-entry / quine zip bombs**: each local header's "compressed data"
   region can be made to overlap the same physical bytes, yielding enormous
   effective ratios from a small file. Orthogonal to the header-identity question —
   budget-based per-entry + total-decompressed caps catch it regardless, because
   they bound bytes *materialized*, not bytes *declared*. (Primary source, David
   Fifield's "A better zip bomb", 403s automated fetch; ratio magnitudes are
   secondary-sourced.)

7. **Sibling-repo precedent — this exact question is already decided in-house.**
   `ocx-mirror`'s `crates/ocx_python/src/repack.rs` reads an already-downloaded
   wheel fully (`std::fs::read`), computes SHA-256, and only *then* opens it via
   `zip::ZipArchive::new(Cursor::new(bytes))` — never `read_zipfile_from_stream`.
   It enforces a per-entry cap and a running `MAX_TOTAL_DECOMPRESSED_BYTES` (1 GiB)
   via `read_entry_capped` (`take(remaining + 1)`, reject if over — **never trusts
   the entry's declared size**) and rejects zip-slip via `enclosed_name()`.

8. **Industry pattern — containerd content store**: two-phase ingest. The *write*
   phase streams the blob into an ingest location while hashing; the *commit* phase
   compares the computed digest against the expected one and only then atomically
   renames into the content-addressed store. Nothing is trusted as "the blob"
   until digest verification completes.

9. **ocx's current tar pipeline is already this pattern**, single-pass only because
   tar needs no `Seek`. Zip's `Seek` requirement is precisely where that
   architecture cannot extend without a structural change.

## Recommendation

Reuse the existing pipeline's compressed-side stages verbatim —
`HashingAsyncReader`, `ProgressReader`, the `take(layer.size)` cap — but terminate
them into a **temp file on local disk** rather than into `extract_tar_from_reader`.
At EOF, compare the digest; on mismatch delete the temp file and return
`DigestMismatch` **before any zip parsing happens**. No bytes are ever interpreted
as zip structure until proven to be the bytes the manifest named — this ordering is
the security-critical part, and it matches the existing completeness→digest
ordering in `pull_layer_with_caps`.

Only then open the temp file with `zip::ZipArchive::new(File)` and extract via the
**central directory exclusively** — never `read_zipfile_from_stream`. Enforce a
per-entry decompressed cap and a running total cap while extracting (**port
`repack.rs`'s `read_entry_capped` rather than inventing a new one**), reject entries
failing `enclosed_name()`, and treat central-directory metadata as the only trusted
source — no fallback to local headers for anything security-relevant.

**Additionally, per CVE-2025-54368 (§4b): digest verification is not enough.** A
single archive with a single digest can be read differently by two conformant-ish
parsers. Adopt uv's post-CVE posture as an acceptance criterion — reject an archive
whose local entries disagree with the central directory, whose CRC or sizes fail to
validate, or which carries a dangling local entry with no central-directory header.

Cost: one extra disk write plus one extra local read pass versus tar's single pass.
That cost buys not trusting an unauthenticated on-the-wire structure.

## Three orthogonal zip risk classes (do not conflate)

| Class | Vector | Status in ocx |
|---|---|---|
| Parser differential | local-header vs central-directory disagreement (CVE-2013-4787, CVE-2025-54368) | **Unaddressed** — no cross-validation exists |
| Zip-slip / traversal | escaping entry names, symlink-mediated traversal (CVE-2025-29787, fixed in `zip` 2.3.0; vendored here is 8.6.0) | `enclosed_name()` used, but a failure **silently `continue`s** instead of erroring (`archive/zip.rs:236-238`); symlink targets validated via `symlink::validate_target` |
| Decompression bomb | overlapping entries, quines, high-ratio entries | **Unaddressed for zip** — no cap; the tar path's 100×/256 MiB cap does not cover it |

## Sources

[docs.rs zip read_zipfile_from_stream](https://docs.rs/zip/0.5.13/zip/read/fn.read_zipfile_from_stream.html) ·
[zip-rs/zip2](https://github.com/zip-rs/zip2) ·
[async_zip stream module](https://docs.rs/async_zip/latest/async_zip/base/read/stream/index.html) ·
[bearcove/rc-zip](https://github.com/bearcove/rc-zip) ·
[CVE-2013-4787](https://nvd.nist.gov/vuln/detail/CVE-2013-4787) ·
[Ostorlab ZIP exploitation](https://blog.ostorlab.co/zip-packages-exploitation.html) ·
[UBOS zip bomb overview](https://ubos.tech/news/understanding-zip-bombs-construction-risks-and-mitigation-2/) ·
[containerd content store](https://pkg.go.dev/github.com/containerd/containerd/content) ·
Local: `ocx-mirror/crates/ocx_python/src/repack.rs`, `ocx-sion/crates/ocx_lib/src/oci/client.rs`
