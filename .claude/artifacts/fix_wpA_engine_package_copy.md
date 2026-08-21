# WP-A (engine) — review-fix log for `ocx package copy`

Worktree `.agents/worktrees/engine`, branch `hex/pkgcopy-fix--engine`, based on
`evelynn @ dfcdcb98`.

Scope: `oci/copy.rs`, `oci/client/transport.rs`, `oci/client/native_transport.rs`,
`oci/client/test_transport.rs`. Out-of-set edits are listed under
[Handoffs](#handoffs) — none was optional; each is what makes the workspace
compile after an in-scope change.

## Findings

| # | Severity | Finding | Status |
|---|---|---|---|
| 1 | BLOCK | Served leaf digest never compared to the requested one | fixed |
| 2 | HIGH | `blob_digests` built with no dedup, two sites | fixed |
| 3 | HIGH | Spool unbounded; `layer.size`/`config.size` discarded | fixed |
| 4 | HIGH/WARN | `push_blob_from_path` defaulted to a whole-file buffer | fixed |
| 5 | WARN | `verify_spooled_blob` is a second full read + hash | fixed (folded into the spool) |
| 6 | WARN | Referrer caps warn, then report success | fixed |
| 7 | WARN | Scratch lands in `std::env::temp_dir()` | fixed at this layer; caller handoff open |
| 8 | SUGGEST | `as` narrowing, F-9, F-10, N-1, A-1, fan-out abort | fixed (all six) |
| 9 | TESTS | Byte-identity claim, referrer counting, missing spool-corruption test | fixed |

---

### 1 — BLOCK: the served leaf digest was never compared to the requested one

`crates/ocx_lib/src/oci/copy.rs:180-197`.

`fetch_manifest_raw_bytes_addressed` checks the bytes against the digest the
registry filed them under, which is self-consistency, not identity: a source
answering a request for A with a coherent manifest for B passed. B would be
pushed to the target while the caller merged A into the target's index — an
index entry naming a manifest nobody uploaded, reported as success (CWE-345).

Fixed at the copy site, independently of WP-B's parallel fix in `oci/client.rs`:

```rust
if &digest != leaf_digest {
    return Err(ClientError::DigestMismatch { expected: …, actual: … });
}
```

The same check now also guards every referrer manifest against the digest its
referrers listing named (`copy.rs`, `Transfer::copy_referrers`).

Proved by `a_manifest_served_under_the_wrong_digest_is_refused`, red with the
comparison removed — see [Red runs](#red-runs).

### 2 — HIGH: duplicate blob digests collided on one spool path

Both build sites (`copy_leaf`, and the referrer loop) constructed
`Vec<Digest>` with no dedup. Every entry spools to `scratch/<hex>`, so two
concurrent tasks for one digest write and delete the same path — one truncates
the file the other is uploading. An empty config reused as a layer is the
ordinary shape that produces it.

Both sites now go through one `blob_set(image, subject) -> Vec<BlobRef>`
(`copy.rs`), which dedups on the digest string. No in-tree helper fits
(IDIOM-11): `VecExt::unique_clone` sorts and needs `Ord + Clone` on the whole
element, and dedup here is on one field of a struct that also carries a size.

### 3 — HIGH: the spool was unbounded

`layer.size` and `config.size` were parsed and dropped. `pull_blob_to_file`
writes whatever the source sends, so a source under-declaring a layer filled the
scratch filesystem.

`BlobRef { digest, size: u64 }` now carries the declared size to the transfer,
and `Transfer::spool` bounds the read with `.take(declared + 1)` (PKG-05,
PKG-07). The extra byte is deliberate: an over-long body reaches the digest check
as a genuine mismatch rather than being truncated to the cap and hashed as if it
were the whole blob.

An absurd declaration is refused by `blob_set` — the one place the descriptor is
read — against a new `MAX_COPIED_BLOB_BYTES` (8 GiB), reusing the existing
`ClientError::LayerSizeExceeded`. It started in `spool`, one blob at a time;
writing the test showed that a set holding one absurd declaration had already
uploaded its *siblings* before the refusal fired, because the fan-out runs to
completion by design. Clamping at the parse boundary refuses the set whole,
before any HEAD (`BlobRef.size` is `u64` precisely so the clamp cannot be
skipped downstream).

In-tree precedent followed: `Client::pull_layer`'s `.take(layer.size)` plus its
`ShortBlobRead`-before-`DigestMismatch` ordering, which `spool` reproduces and
cites.

### 4 — HIGH/WARN: `push_blob_from_path` had a whole-file-buffering default

`transport.rs:201-213`. The default read the file into memory and delegated to
`push_blob` — the exact allocation the method exists to avoid — so a transport
that never noticed the method compiled and silently reintroduced it.

The method is now required, with a `# No default` doc block stating why, and
contrasting `mount_blob`, whose default *is* semantically correct. A test double
that genuinely does not care writes one line against the new
`transport::push_blob_buffered` helper (`#[cfg(test)]`), where buffering is
stated rather than inherited.

Cost: eight test doubles across six files needed one line each. See Handoffs.

### 5 — WARN: `verify_spooled_blob` was a second full read

The reviewers offered (a) delete it, or (b) keep it with a corrected rationale.
Neither was taken; a third option is strictly better and is what landed.

`Transfer::spool` now hashes in the same pass that writes — `pull_blob_streaming`
→ `.take(declared + 1)` → `HashingAsyncReader` → file — and `verify_spooled_blob`
is deleted. This keeps (a)'s halving of per-layer disk I/O **without** needing
(a)'s prerequisite: option (a) required writing a verification guarantee into
`OciTransport::pull_blob_to_file`'s doc, and that guarantee is currently false
for `StubTransport`, so (a) as offered would have documented a promise the test
double does not keep.

It also gives finding 3's byte bound somewhere to live: a separate verify pass
has no bytes left to refuse by the time it runs.

`StubTransport::pull_blob_streaming` serves from the same `blobs` map as
`pull_blob_to_file`, so the existing suite exercises the new path unchanged.
Proved by `a_spooled_blob_that_rehashes_wrong_never_reaches_the_target` — see
[Red runs](#red-runs).

### 6 — WARN: a referrer cap trip warned, then reported success

`copy.rs:336-339` and `:347-350` logged `warn!` and returned `Ok`. A promotion
that dropped a signature past its cap left the target holding an artifact
verifiable at the source and unverifiable at the target, exit code 0.

Both are now `ClientError::TraversalLimitExceeded { limit_kind, limit, actual,
subject }` — one new variant, one new `TraversalLimit` enum, covering all three
caps (referrer depth, referrers per leaf, blobs per manifest) rather than three
variants (PKG-11, error-type economy). Classifies to `DataError` (65).

### 7 — WARN: scratch landed in `std::env::temp_dir()`

`copy_leaf` gained `scratch_root: Option<&Path>` and uses
`tempfile::tempdir_in(root)` when supplied. `None` keeps `$TMPDIR` with a
`// ponytail:` note naming the call site that should pass a real root — this is
a placeholder, not a choice: `$TMPDIR` is memory-backed on most Linux hosts,
which is exactly what spooling to disk exists to avoid.

The parameter is threaded no further than this layer. See Handoffs.

### 8 — SUGGEST: all six applied

- **`as` narrowing** — `native_transport.rs`, `do_push_blob`: `Some(total as usize)`
  → `usize::try_from` with `LayerSizeExceeded` on failure (PKG-03). A narrowing
  cast on a 32-bit target hands the fork a short length; its `while remaining > 0`
  loop uploads a prefix and the failure surfaces as a rejected committing PUT
  rather than as the size problem it is.
- **F-9, an image-index referrer** — refused. Pushing one attaches a referrer
  whose children were never copied: present in the target's listing, unfetchable
  behind it.
- **F-10, no entry-count cap on the blob set** — `MAX_BLOBS_PER_MANIFEST` (512),
  checked in `blob_set` before `Vec::with_capacity`, so the capacity is sized
  from the clamped count and never the raw declaration (PKG-04, PKG-06).
- **N-1, stray bare blocks** at `:335`/`:397` — gone with the method conversion.
- **A-1, four functions threading `source`/`target`** — `struct Transfer<'a>`,
  constructed once in `copy_leaf`, five `&self` methods (ARCH-01). The two
  identifiers are the same type, so transposing them was a one-token edit with no
  compiler objection behind it; binding them in the one function that knows which
  is which makes the transposition unrepresentable.
- **Blob fan-out `.collect()` has no early abort** — deliberate, now stated in
  `copy_blobs`'s doc: fail-fast on the report, run-to-completion on the work
  (PKG-23). Cancelling a push abandons an open upload session at the target, and
  a blob that did land is one the caller's retry finds present.

### 9 — TESTS

**Byte-identity test could not fail.** Its docstring claimed comparing bytes
rather than parsed values catches a re-serialising copy; it could not, because
the fixture was itself produced by serde's compact writer, so a re-serialising
copy reproduced it byte for byte. The fixture is now seeded pretty-printed —
a shape that writer never emits — through a new `seed_raw`. The test also
asserts the fixture differs from the compact encoding, so the fixture cannot
silently drift back to canonical and take the assertion with it.

**Recursive-referrer test counted rather than identified.** Two is equally what
copying the SBOM twice produces. Both referrers are now asserted by digest at
the target, and asserted *before* the count — ordering matters, because the
count fires first otherwise and the identity assertion never gets to be the one
that reds.

**New: `a_manifest_served_under_the_wrong_digest_is_refused`** (finding 1). The
substitute names the *same* blobs on purpose: every one is already at the source,
so without the guard the copy runs to completion and reports success. The first
draft used a substitute naming an unseeded blob — it went red, but through a
downstream `ShortBlobRead`, which proves nothing about the guard under test.

**New: `a_spooled_blob_that_rehashes_wrong_never_reaches_the_target`**
(finding 5). Corruption is same-length by construction, so the read is complete
and this is unambiguously a content failure rather than a truncated transfer.
Asserts no `push_blob:` for that digest and no manifest at the target.

**New: `a_blob_declaring_an_absurd_size_is_refused_before_it_is_fetched`**
(finding 3) and **`a_manifest_naming_more_blobs_than_the_cap_is_refused`**
(findings 6, 8/F-10). The second also asserts the exit code is `DataError`,
because the point of the change is that a cap trip is a failure at all.

## Verification

```
cargo fmt                                            # clean
cargo check --workspace --all-targets --locked       # Finished, 0 errors
cargo clippy -p ocx_lib --all-targets -- -D warnings # no issues
cargo nextest run -p ocx_lib copy                    # 23 tests run: 23 passed
```

The 23 are 13 `oci::copy::tests` (10 pre-existing, 4 new, 1 rewritten fixture)
plus 7 `publisher::copy::tests` unmodified except for the `None` scratch-root
argument, and 3 incidental name matches.

Not run, per the task: `task verify` (the orchestrator's merge gate).

### Red runs

Every new or repaired check was mutated and observed red, then restored and the
restore verified by `sha256sum` against a pre-mutation snapshot (a restore that
silently failed would leave the next green meaningless).

| Mutation | Test | Red at |
|---|---|---|
| `if &digest != leaf_digest` → `if false` | `a_manifest_served_under_the_wrong_digest_is_refused` | the `expect_err` — without the guard the copy *succeeds* |
| `if actual != blob.digest` → `if false` | `a_spooled_blob_that_rehashes_wrong_never_reaches_the_target` | the `expect_err` |
| `push_manifest_raw(…, leaf_bytes)` → `push_manifest_raw(…, serde_json::to_vec(&manifest))` | `leaf_manifest_bytes_survive_the_copy_verbatim` | the verbatim-bytes assertion |
| recursion replaced by `saturating_add(0)` | `referrers_are_copied_recursively_and_only_when_asked` | `signature at the target`, the identity assertion |
| size clamp `.filter(…)` → `.filter(\|_\| true)` | `a_blob_declaring_an_absurd_size_is_refused_before_it_is_fetched` | the `expect_err` |
| `if declared > MAX_BLOBS_PER_MANIFEST` → `if false` | `a_manifest_naming_more_blobs_than_the_cap_is_refused` | the `expect_err` |

## Handoffs

**H1 — `scratch_root` is not threaded past `oci::copy` (finding 7, WP-B + WP-D).**
`publisher/copy.rs:205` passes `None`. The CLI
(`crates/ocx_cli/src/command/package_copy.rs`) owns the `FileStructure` and
therefore the `TempStore` root; it should travel down through
`publisher::copy::CopyRequest` to `copy_leaf`. Until it does, every promoted
layer spools to `$TMPDIR`.

**H2 — out-of-set edits, all compile-forced.** Making
`push_blob_from_path` required (finding 4) and adding
`ClientError::TraversalLimitExceeded` (finding 6) do not compile without these.
None changes behaviour of the file it touches.

| File | Edit |
|---|---|
| `oci/client/error.rs` | `TraversalLimitExceeded` variant, `TraversalLimit` enum, `DataError` classification arm |
| `oci/client.rs` | `#[cfg(test)] pub(crate) use transport::push_blob_buffered;`; `push_blob_from_path` on two test doubles |
| `oci/verify/pipeline.rs` | `push_blob_from_path` on two doubles (`unimplemented!` — verify never pushes a file-backed blob) |
| `oci/attest/pipeline.rs` | one double, delegates to `push_blob_buffered` |
| `oci/sign/pipeline.rs` | one double, delegates to `push_blob_buffered` |
| `oci/referrer/capability.rs` | one double (`unimplemented!`) |
| `project/resolve.rs` | `TraversalLimitExceeded` arm added to the exhaustive `classify` match |

**H3 — `ClientError::DigestMismatch` carries `String`, not `Digest`.** Finding 1
and the referrer identity check both format both sides to compare-and-report. Not
worth changing inside this WP's scope; worth noting for whoever next touches that
variant.
