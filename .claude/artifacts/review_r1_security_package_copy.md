# R1 Security Review — `ocx package copy`

- **Focus:** security
- **Baseline:** `main` (0ed4a446) -> `evelynn` (73939e30)
- **Scope:** `crates/ocx_lib/src/oci/copy.rs`, `crates/ocx_lib/src/publisher/copy.rs`,
  `crates/ocx_lib/src/oci/client/{transport.rs,native_transport.rs}`,
  `crates/ocx_lib/src/oci/client.rs`,
  `crates/ocx_cli/src/command/{package_copy.rs,package_describe.rs}`,
  `crates/ocx_cli/src/api/data/package_copy.rs`
- **Verdict:** needs work — 1 Block, 1 High, 6 Warn, 2 Suggest

Stage 1's confirmed finding (`publisher/copy.rs:285` `list_tags` -> `Mirrored`) is **not**
re-reported. Two further Invariant #5 violations were found at other call sites.

---

## Read-site audit — every read in the new code

`W?` = does this read's answer decide, gate, or verify a write?

| # | Site | Addressing | Resolves via | W? | Verdict |
|---|---|---|---|---|---|
| 1 | `oci/copy.rs:126` `fetch_manifest_raw_bytes_addressed(&source_leaf, Canonical)` | Canonical | explicit | yes — bytes become the target manifest | correct |
| 2 | `oci/copy.rs:206` `ReferrersApiCapability::probe(transport, &image, subject)` | Canonical | `image = transport_write_reference(target)` (`client.rs:370`) | yes — gates the referrer PUTs | correct |
| 3 | `oci/copy.rs:256` `transport.head_blob(&target_image, digest)` | Canonical | `transport_write_reference` | yes — decides upload vs skip | correct |
| 4 | `oci/copy.rs:282` `transport.pull_blob_to_file(&source_image, ...)` | Canonical | `read_reference(source, Canonical)` (`copy.rs:250`) | yes — bytes become the target blob | correct |
| 5 | `oci/copy.rs:344` `transport.list_referrers(&source_image, subject, None)` | Canonical | `read_reference(source, Canonical)` (`copy.rs:341`) | yes — decides which referrers are PUT | correct |
| 6 | `oci/copy.rs:356` `fetch_manifest_raw_bytes_addressed(&referrer_id, Canonical)` | Canonical | explicit | yes — bytes become the target referrer | correct |
| 7 | `publisher/copy.rs:308` `fetch_manifest_raw_bytes_addressed(source, Canonical)` | Canonical | explicit | yes — chooses the leaf set to copy | correct |
| 8 | `publisher/copy.rs:294` `fetch_manifest_raw_bytes_addressed(&identifier, Canonical)` (`leaf_size`) | Canonical | explicit | yes — the `size` written into the target index | correct |
| 9 | `publisher/copy.rs:373` `fetch_manifest_raw_bytes_addressed(target, Canonical)` (`read_target_entries`) | Canonical | explicit | yes — the disposition report and the `--dry-run` answer | addressing correct; see F-3 for its error handling |
| 10 | `publisher/copy.rs:285` `client.list_tags(request.target.clone())` | **Mirrored** | `client.rs:409-411` | yes — which rolling tags are PUT | **VIOLATION** (Stage 1, not re-reported) |
| 11 | `publisher/copy.rs:286` `cascade::resolve_cascade_tags(...)` -> `cascade.rs:313` `client.fetch_manifest(&blocker_id)` | **Mirrored** | `client.rs:458` `transport_reference` | yes — decides whether a rolling tag at the canonical target moves | **VIOLATION** — F-1 |
| 12 | `publisher/copy.rs:237` `merge_platform_into_index` -> `client.rs:505` `pull_manifest_raw(&ref_)` | Canonical | `client.rs:499` `canonical_reference()` | yes — read-modify-write of one index | correct |
| 13 | `publisher/copy.rs:253` `push_canonical_tag` -> `client.rs:669` `pull_manifest_raw(&digest_ref)` | Canonical | `client.rs:665-668` | yes — bytes re-PUT under the canonical tag | correct |
| 14 | `cli/command/package_copy.rs:144` `publisher.pull_description(&source, ...)` | **Mirrored** | `client.rs:1720` `transport_reference` | yes — feeds `push_description(target)`, canonical (`client.rs:1550`) | **VIOLATION** — F-2 |
| 15 | `cli/command/package_describe.rs:161` `publisher.pull_description(&source, ...)` (`--from`) | **Mirrored** | `client.rs:1720` | yes — same | **VIOLATION** — F-2 |

**Totals: 15 read sites — 11 correct, 4 violating** (one of which is Stage 1's, already known).

A module-level `Canonical` import was not accepted as evidence for any site; each row was
resolved down to the function that actually builds the `native::Reference`.

---

## Actionable

### F-1 [Block] `crates/ocx_lib/src/publisher/copy.rs:286` — cascade blocker probe reads through the mirror

`target_tags` calls `cascade::resolve_cascade_tags(client, request.target, ...)`. That walks
`has_blocking_platform` (`crates/ocx_lib/src/package/cascade.rs:305-320`), whose only registry
call is `client.fetch_manifest(&blocker_id)` — and `client.rs:458` builds that reference with
`transport_reference`, i.e. **`ReadAddressing::Mirrored`**. Its answer decides whether `3.28`,
`3` and `latest` at the **canonical** target are re-pointed at the promoted digest
(`publisher/copy.rs:235-249`).

Concrete failure: prod already publishes `3.29` for `linux/amd64`, so `latest` must not move
when `3.28.1` is promoted. A stale or poisoned mirror of the prod host answers the `3.29`
manifest fetch with a manifest that lacks `linux/amd64`; `has_blocking_platform` returns
`Ok(false)` (`cascade.rs:314-319`), `resolve_cascade_tags` pushes `latest`
(`cascade.rs:239-244`), and `latest` at prod is moved **backwards** to `3.28.1`. The asymmetry
that makes this exploitable rather than merely wrong: an `Err` from the blocker fetch stops the
cascade conservatively (`cascade.rs:221-225`), but a *successful* answer that merely omits the
platform is taken at face value — the mirror does not need to fail, it needs to under-report.
CWE-345 / CWE-367; `subsystem-oci.md` Invariant #5.

The mirrored read itself lives in unchanged code, and the pre-existing `push_with_cascade`
(`cascade.rs:273`) shares the shape — but this diff adds a new caller that hands it a
*promotion target* rather than the registry it just pushed to, which is exactly the case the
invariant names.

**Remediation:** thread `ReadAddressing` through `resolve_cascade_tags` -> `has_blocking_platform`
and add `Client::fetch_manifest_addressed` beside the three that already exist
(`list_tags_addressed` `client.rs:417`, `probe_manifest_digest_addressed` `client.rs:1949`,
`fetch_manifest_raw_bytes_addressed` `client.rs:2014`). Default stays `Mirrored` so the push
path is unchanged; the copy path passes `Canonical`. Fixed together with Stage 1's
`list_tags:285`, `target_tags` becomes wholly canonical.

### F-2 [High] `crates/ocx_cli/src/command/package_copy.rs:144`, `package_describe.rs:161` — description read from a mirror, published to the canonical target

`Client::pull_description` (`crates/ocx_lib/src/oci/client.rs:1720`) reads through
`self.transport_reference(&desc_identifier)` — **Mirrored**. Its sibling
`Client::push_description` (`client.rs:1550`) writes through `canonical_reference()`, with the
comment "Push stays canonical (mirror-free)". Both new call sites read with the first and write
with the second:

- `package_copy.rs:144-145` (`--description`)
- `package_describe.rs:160-164` (`--from`, new in ab4bbcda)

So a poisoned or stale mirror of the *source* registry chooses the README body, the logo bytes
and the `org.opencontainers.image.*` / `sh.ocx.keywords` annotations that are then published to
production under the operator's push credentials. The README is the package's public catalog
page; a substituted install snippet is a plausible payload. CWE-345; Invariant #5.

Tagged High rather than Block because the payload is catalog prose, not the promoted artifact:
no digest, lock pin or signature subject is affected.

**Remediation:** add `Client::pull_description_addressed(identifier, temp_dir, addressing)` with
`pull_description` delegating at `Mirrored` — the same split as `list_tags` /
`list_tags_addressed` at `client.rs:409-421` — and pass `Canonical` from both new call sites.

### F-3 [Warn] `crates/ocx_lib/src/publisher/copy.rs:381-384` — `read_target_entries` swallows every error into "the target has nothing"

```rust
Ok(None) => return Ok(entries),        // 378 — correct: the tag is new
Err(error) => {                        // 381
    log::debug!("Target {target} has no readable index yet ({error})");
    return Ok(entries);
}
```

`fetch_manifest_raw_bytes_addressed` already maps `ManifestNotFound` to `Ok(None)`
(`client.rs:2044`), so the `Err` arm catches only *genuine* failures: auth denial, a transient
5xx, an SSRF refusal, a `DigestMismatch` on the target's own index, a malformed index. All of
them become an empty entry list, which drives the per-platform disposition at
`publisher/copy.rs:186-190`: every row reports `Added` instead of `Replaced`, and every
`KeptNotInSource` row (`:212-220`) disappears.

The doc comment's defence — "Anything worse surfaces on the first write" — does not hold under
`--dry-run`, which skips every write (`:197-199`). `--dry-run` is the review step before a
production promotion, and this is exactly where it will report "adding 3 platforms to an empty
target" for a target that already publishes them. ERR-19 / IDIOM-04: a discarded `Result` whose
stated reason covers only one of the error classes it actually absorbs.

**Remediation:** propagate the `Err` (the absent-tag case is already `Ok(None)`), or at minimum
route it through `context.ui().warn` and mark the affected rows as unknown rather than `Added`.

### F-4 [Warn] `crates/ocx_lib/src/oci/copy.rs:278-283` — blob spool has no byte cap

`copy.rs:142-146` builds `blob_digests` from the source manifest and **discards each
descriptor's declared `size`**. `copy_blob` then calls
`transport.pull_blob_to_file(&source_image, digest, &spooled)` with no ceiling;
`NativeTransport::pull_blob_to_file` delegates to the fork's `Client::pull_blob`
(`external/rust-oci-client/src/client.rs:1443-1454`), which streams the whole response body into
the file, digesting as it goes and validating only after the stream ends.

A hostile or compromised source registry therefore writes an arbitrary number of bytes into
`$TMPDIR` per blob, four blobs concurrently (`MAX_CONCURRENT_BLOB_TRANSFERS`, `copy.rs:34`),
before `verify_spooled_blob` (`copy.rs:283`) ever runs. Local disk exhaustion; CWE-400,
PKG-05 / PKG-07.

The existing pull path does bound this: `Client::pull_layer` caps the raw stream at `layer.size`
via `.take()` (`subsystem-oci.md`, "Decompression-bomb caps": "Compressed | `layer.size` bytes
via `.take()`"). The copy path is a new ingestion path that drops that cap while having the same
`size` field available in the manifest it just parsed.

**Remediation:** carry `(Digest, i64)` pairs out of `copy.rs:142-146`, and either add a
size-capped `pull_blob_to_file` or spool via `pull_blob_streaming` +
`AsyncReadExt::take(size + 1)`, refusing above the declared size.

### F-5 [Warn] `crates/ocx_lib/src/oci/copy.rs:229-231` — no dedupe before the blob fan-out; duplicate digests race on one spool path

`blob_digests` (`copy.rs:142-146`, and again at `:371-375` for referrers) is built by pushing the
config digest and then each layer digest, with no dedupe. `copy_blobs` fans out with
`futures::stream::iter(digests).map(copy_blob).buffer_unordered(4)`.

A manifest listing the same digest twice — trivially constructible by a hostile source, and
reachable innocently when two layers are byte-identical — runs two concurrent `copy_blob` calls
against the *same* path `scratch.join(hex)`: both truncate-and-write it (`copy.rs:278`, `:282`),
both hash it (`:283`), both upload it (`:286`), and both `remove_file` it (`:290`). Observable
outcomes: `verify_spooled_blob` hashing a file a sibling task is truncating;
`push_blob_from_path` stat-ing and re-opening a path a sibling has already unlinked;
`BlobTransfers.uploaded` double-counting. CWE-367, self-inflicted rather than cross-user (see Q6
below on directory permissions).

No *unverified* bytes reach the target: the final upload is digest-addressed and a
spec-compliant registry rejects a mismatch. The impact is a spuriously failed or double-counted
promotion.

**Remediation:** dedupe before the fan-out — a `BTreeSet<&Digest>`, or `VecExt::unique_clone`
which is already in the prelude.

### F-6 [Warn] `crates/ocx_lib/src/oci/copy.rs:336-339, 347-350` — hitting a referrer cap is a `warn!` and a success

```rust
if depth >= MAX_REFERRER_DEPTH { log::warn!(...); return Ok(0); }        // 336-339
if seen.len() >= MAX_REFERRERS_PER_LEAF { log::warn!(...); break; }      // 347-350
```

Both caps are correctly enforced (see Q4 below), but exhausting either truncates the referrer
chain and the copy still returns `Ok`, reports `status: "copied"`, and exits 0. A signature or
attestation that stayed behind is precisely the outcome `ensure_target_serves_referrers`
(`copy.rs:196-215`) exists to prevent — the comment at `copy.rs:171-176` states that refusing is
required so the tool never "quietly promot[es] an unsigned artifact". The caps reintroduce that
failure mode on a different trigger, at `warn` level on stderr, with no signal in `CopyOutcome`
(`publisher/copy.rs:137-151`) or in `CopyReport`.

**Remediation:** return a typed error on cap exhaustion, or carry `truncated` /
`referrers_skipped` through `LeafCopy` -> `CopyOutcome` -> `CopyReport` and classify a truncated
copy as a non-zero exit.

### F-7 [Warn] `crates/ocx_cli/src/api/data/package_copy.rs:95` — registry-controlled platform text reaches stdout unsanitized

`CopyReport::print_plain` passes `row.platform` (and `row.digest`, `row.disposition`) straight
into `data.print_table`, which performs no neutralisation
(`crates/ocx_lib/src/cli/data_interface.rs:213-222`).

The platform string is registry-controlled:

- `TryFrom<native::Platform> for Platform` (`crates/ocx_lib/src/oci/platform.rs:791-802`) takes
  `variant: platform.variant` and `os_features` **verbatim** from the source or target index
  entry — only `os` and `architecture` are constrained, by closed enums.
- `Platform`'s `Display` (`platform.rs:514, 527`) renders them through
  `escape_platform_component`, whose own doc says "All other bytes pass through verbatim"
  (`platform.rs:629-631`) — it percent-escapes `%`, `/`, `+`, `,` and nothing else.

So an index entry whose `variant` embeds an ESC-introduced CSI sequence produces a report row
carrying that sequence live on stdout, reached from either the source index
(`publisher/copy.rs:326`) or the target index (`publisher/copy.rs:391`). CWE-150;
`security.md` SEC-31 / SEC-34.

The error boundary *is* covered (`crates/ocx_cli/src/main.rs:37`), and sibling report types in
the same directory already sanitize at the print site — `api/data/index.rs:215-216`,
`api/data/attestation.rs:100-104`. This one does not.

**Remediation:** wrap the three cell values in `crate::api::data::sanitize_for_terminal`,
matching `attestation.rs:100-106`.

### F-8 [Warn] `crates/ocx_lib/src/oci/copy.rs:126-140` — the requested leaf digest is never compared to the served one

`copy_leaf` fetches `source_leaf` (digest-addressed with `leaf_digest`), binds the returned
`digest`, and pushes at `target.without_tag().clone_with_digest(digest)` (`copy.rs:154`) —
`leaf_digest` and `digest` are never compared. The module's entire premise ("the leaf is copied
verbatim ... its digest is load-bearing twice over", `copy.rs:6-12`) rests on them being equal.

They are equal today, but only because the vendored fork enforces it one layer down:
`_pull_manifest_raw` calls `validate_digest(&body, digest_header, image.digest())`
(`external/rust-oci-client/src/client.rs:1258`), and `validate_digest`
(`external/rust-oci-client/src/digest.rs:122-143`) hashes the body against the *reference*
digest as well as against the header. ocx's own `verify_raw_bytes_digest`
(`client.rs:2169-2178`) checks only body-against-*claimed*, and a registry controls both sides
of that.

The `OciTransport::pull_manifest_raw` contract (`transport.rs:80-85`) promises nothing about
requested-digest validation, so the property is an implementation detail of one impl, unasserted
anywhere in this diff. Were it to regress, the target would receive a manifest under a digest the
caller never asked for, while `publisher/copy.rs:241` writes the *requested* digest into the
target index — an index entry pointing at a manifest that was never uploaded, reported as
success.

**Remediation:** after `copy.rs:129`, compare and fail:
`if &digest != leaf_digest { return Err(ClientError::DigestMismatch { expected: leaf_digest.to_string(), actual: digest.to_string() }); }`.
Three lines that make the module self-defending. The same check is worth adding after
`copy.rs:357` against `referrer_digest`.

---

## Suggest

### F-9 [Suggest] `crates/ocx_lib/src/oci/copy.rs:370-377` — an image-index referrer is pushed with its children uncopied

`if let super::Manifest::Image(image) = &manifest { ... copy_blobs(...) }` — blobs are copied
only for the `Image` arm, but `push_referrer_manifest` (`copy.rs:379-381`) runs unconditionally.
A referrer that is itself an image index lands at the target naming child manifests that were
never transferred. Cosign bundles and DSSE in-toto attestations are image manifests, so this is
not reachable by any artifact ocx produces — but the source registry chooses the shape.

**Remediation:** add an explicit `Manifest::ImageIndex` arm that either refuses (matching the
leaf-level refusal at `copy.rs:136-140`) or recurses over `index.manifests`.

### F-10 [Suggest] `crates/ocx_lib/src/oci/copy.rs:142-148` — no entry-count cap on the blob set

`blob_digests` is bounded only by `MAX_INDEX_DOCUMENT_BYTES` (32 MiB, `client.rs:38`) — on the
order of 300k minimal layer descriptors, each costing at least one HEAD against the target
registry. PKG-06 asks for an explicit entry-count cap alongside the byte cap ("one 5 GB member
and a million 5 KB members are different attacks"). The pre-existing pull path shares the shape,
so the copy path inherits this gap rather than introducing it.

**Remediation:** a named `MAX_LAYERS_PER_MANIFEST` checked in `copy_leaf`, with a typed error
carrying the limit and the actual count (PKG-11).

---

## Deferred

(none)

Every finding above has a determinate remediation that needs no human input. The two
judgement-shaped questions — whether a truncated referrer chain should be fatal (F-6), and
whether F-2 warrants Block rather than High — are argued in place from the module's own stated
contract rather than left open.

---

## Verified safe (the orchestrator's nine questions)

**Q1 — digest verification before use.** No path puts unverified bytes at the target.
*Blobs:* `pull_blob_to_file` -> `verify_spooled_blob` (`copy.rs:283`, re-hashes the whole file
with `HashingAsyncReader`) -> `push_blob_from_path` (`copy.rs:286`). Verification precedes the
upload with no window an external process can use; F-5 covers the intra-process race.
*Manifests:* `fetch_manifest_raw_bytes_capped` refuses over-cap bodies before parsing
(`client.rs:2051-2056`), then runs `verify_raw_bytes_digest` (`client.rs:2061`); the fork
additionally checks the *requested* digest (`client.rs:1258` -> `digest.rs:122-143`) — F-8
explains why that must not be relied on silently. *Referrer manifests:*
`push_referrer_manifest` (`native_transport.rs`) re-hashes the bytes itself and PUTs
digest-addressed at `sha256(bytes)`.

**Q2 — cross-repository blob mount.** Sound. `copy.rs:264-272` mounts only when
`source.registry() == target.registry()`, and `mount_source_reference`
(`crates/ocx_lib/src/oci/identifier.rs:248-250`) sends the source repository as the `from=`
parameter of an upload POST against the *target* repository on the *canonical* host. The registry
performs the mount internally by digest, so content addressing supplies the integrity guarantee
that local re-hashing would; the registry's own authorization decides whether the authenticated
principal may read `from=`, and a refusal degrades to `MountOutcome::UploadRequired`
(`native_transport.rs` maps every mount error that way), never to a wrong blob. The `from=`
value is an `Identifier`-validated repository path (`identifier.rs:102-110`, with a CWE-150-aware
validation test at `identifier.rs:1054-1072`), so no query-parameter injection.

**Q3 — registry-host string comparison.** No spoofing finding. Both operands are user-typed —
`source` from argv, `target` from `--to` / `--identifier` (`package_copy.rs:94-95`) — never
registry-controlled. Every divergence a case fold, an explicit port, a trailing dot or punycode
could introduce makes the comparison *false*, which skips the mount and falls through to the
ordinary verified upload: the fail-safe direction. A false positive would require two different
hosts to be the same string.

**Q4 — referrer recursion.** Bounds are enforced on every path. Depth is checked at function
entry (`copy.rs:336`) before any I/O. The per-leaf count is checked at the top of each loop
iteration (`copy.rs:347`) against a `seen` set threaded by `&mut` through the whole recursion
(`copy.rs:332`), so the 256 ceiling is global rather than per level, and at most 257
`list_referrers` calls are issued. `seen` is keyed on `descriptor.digest`; two *distinct*
referrers have distinct digest strings, so a collision requires string equality — the set can
over-count a differently-spelled duplicate (pushing toward the cap) but can never drop a distinct
referrer. A cycle terminates on `seen`. The source registry cannot inflate the descriptor list
without bound either: the fork caps the referrers index at `MAX_REFERRERS_INDEX_BYTES` before
parsing and refuses above `MAX_REFERRERS_DESCRIPTORS` entries
(`external/rust-oci-client/src/client.rs:2085, 2093-2098`). What is *not* safe is what happens
when a cap is hit — F-6.

**Q5 — bounded ingestion.** No registry-declared size is fed to an allocation. Both
`Vec::with_capacity(image.layers.len() + 1)` sites (`copy.rs:142`, `:371`) size from the parsed
vector's real length, not a wire field, and the body that produced it was refused above 32 MiB
before parsing (`client.rs:2051-2056`) — PKG-04 satisfied. The spool path `scratch.join(hex)`
(`copy.rs:278`) takes `hex` from `Digest::parts()` on an already-parsed digest, and
`Digest::try_from` (`crates/ocx_lib/src/oci/digest.rs:218-236`) enforces exact length plus
`is_ascii_hexdigit`, so no external-origin component reaches the join — PLAT-01 satisfied without
a containment helper. The unbounded axes are byte count (F-4) and entry count (F-10).

**Q6 — scratch directory.** `tempfile::tempdir()` (`copy.rs:120`) creates a randomly named
directory mode `0700`, one per `copy_leaf` call — i.e. one per platform, given the loop at
`publisher/copy.rs:205` — swept by the `TempDir` guard on drop. No SEC-11 predictable-name
exposure and no cross-user symlink race: only the same UID (or root) can reach inside. The only
TOCTOU between `verify_spooled_blob` and `push_blob_from_path` is the intra-process one in F-5.

**Q7 — credential isolation across the two hosts.** No leak. `ensure_auth` is called with `Push`
against the canonical target (`copy.rs:157`, `:254`) and `Pull` against the canonical source
(`copy.rs:280`) on one shared transport, but the fork's token cache keys on
`(registry, repository, operation)` (`external/rust-oci-client/src/token_cache.rs:84-86`), so a
source-host token can never be selected for a target-host request. `Client::ensure_auth`
(`client.rs:397-400`) is exhaustive over `RegistryOperation` and routes `Push` to
`canonical_reference()` unconditionally, so a configured mirror cannot become a push target. No
custom `redirect::Policy` is installed in the fork, leaving reqwest's default, which strips
`Authorization` on a cross-origin redirect.

**Q8 — DSSE attestations (landed 2026-08-20, after this design).** Carried correctly.
`copy_referrers` passes `artifact_type: None` to `list_referrers` (`copy.rs:344`), so it is
media-type agnostic and picks up cosign v3 bundles, in-toto DSSE envelopes and Sigstore
signatures alike. Each referrer travels as **verbatim bytes** (`copy.rs:380` pushes `&bytes`, not
a re-serialisation), so its `subject` descriptor still names the same digest — which is itself
preserved for the leaf, closing the loop for verification at the target. The payload blob travels
with it (`copy.rs:370-377`), and `push_referrer_manifest` re-derives the digest from those exact
bytes, so the target registry indexes the referrer under the same digest. Nested referrers (a
signature over an SBOM) are followed by the recursion at `copy.rs:385-394`. The one shape not
handled is an index-typed referrer — F-9.

**Q9 — untrusted text to the terminal.** The error boundary is covered:
`crates/ocx_cli/src/main.rs:37` renders the whole chain through
`api::data::sanitize_for_terminal`, pinned by a structural test at `main.rs:53-83`. The
`log::warn!` sites in `oci/copy.rs` are safe by construction — `:337` and `:348` interpolate a
`Digest` and a constant, and `:363` interpolates `descriptor.digest` only *after*
`parse_descriptor_digest` (`copy.rs:354`) has proved it is `algorithm:<strict-hex>`. The report
path is not covered — F-7.

---

## Out of scope (noted, not counted as findings)

`crates/ocx_cli/src/command/package_describe.rs:80` — the pre-existing merge path builds
`std::env::temp_dir().join(format!("ocx-describe-{}", std::process::id()))`, a predictable name
in a world-writable directory, then `create_dir_all`s it (SEC-11; CWE-377 / CWE-59 symlink
pre-creation). Unchanged by this diff, and the *new* `copy_from` (`:159`) correctly uses
`tempfile::tempdir()` — worth a follow-up on the old path for consistency.

`crates/ocx_lib/src/oci/client.rs:1752-1787` — `pull_description` writes each description layer
to disk with no byte cap and then loads it whole into memory, the same class as F-4. Unchanged
code; the new `--description` / `--from` call sites route through it.
