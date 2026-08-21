# Review R1 — Architecture: `ocx package copy`

**Reviewer:** architect (opus) · **Round:** 1 · **Focus:** boundary respect, dependency
direction, trade-off honesty · **Verdict:** needs work (no blockers)

Scope read: `crates/ocx_lib/src/oci/copy.rs`, `crates/ocx_lib/src/publisher/copy.rs`,
`crates/ocx_lib/src/oci/client/transport.rs:186-213`,
`crates/ocx_lib/src/oci/client.rs` (`list_tags`, `merge_platform_into_index`,
`push_canonical_tag`, `push_manifest_and_merge_tags`),
`crates/ocx_lib/src/package/cascade.rs:196-320`, `crates/ocx_lib/src/publisher.rs:100-290`,
`crates/ocx_cli/src/command/package_copy.rs`, `crates/ocx_cli/src/error_envelope.rs:227-277`,
`.claude/artifacts/adr_package_copy.md`, `.claude/rules/arch-principles.md`,
`.claude/rules/quality-core.md`, `.claude/rules/subsystem-oci.md`.

Stage 1 findings (the `list_tags` addressing asymmetry, the ADR phase-order contradiction,
the `CopyError` shape) are not re-reported. The architectural judgments underneath them are
in §2, §3 and §4.

---

## 1. Constitution compliance

Checked against `arch-principles.md` per principle, plus `quality-core.md`.

| Principle | Verdict | Note |
|---|---|---|
| Crate layout (`ocx_cli` thin, `ocx_lib` core) | **compliant** | CLI leaf is 173 lines and holds only argument shaping + target resolution. All transfer, merge and cascade logic is in `ocx_lib`. |
| Facade (`Client` over `OciTransport`) | **violation — see F-1** | `oci/copy.rs` drives `client.transport()` directly for seven operations. Each individual bypass is *correct*; the pattern is the finding. |
| Strategy / trait dispatch (`OciTransport`) | **compliant, with F-6** | No new trait. The new defaulted method has a trap. |
| Three-layer errors (`Error → PackageError → PackageErrorKind`) | **violation — see F-4** | `CopyError` is a two-arm transparent wrapper with no kind enum and no identifier context. |
| Command pattern (args → identifiers → task → report data → API) | **compliant** | `package_copy.rs` follows it exactly; report is built from `CopyOutcome`, never from CLI args. |
| Module structure (one concept per file, no `mod.rs`) | **compliant** | `oci/copy.rs`, `publisher/copy.rs`, flat CLI leaf `command/package_copy.rs`. |
| Internal enum exhaustiveness (no `#[non_exhaustive]` on internal non-error enums) | **compliant** | `Disposition` is exhaustive; `CopyError` carries it and error enums are exempt. |
| Where Features Land | **compliant** | CLI command → `command/`, report type → `api/data/`, transfer engine → `oci/`, orchestration → `publisher/`. |
| Utility Catalog ("check before writing a helper") | **compliant** | `verify_spooled_blob` reuses `HashingAsyncReader`; spooling uses `tempfile`; fan-out uses `futures::buffer_unordered`. Nothing hand-rolled that the catalog or a crate already covers. |
| quality-core "Don't Own Non-Domain Code" | **compliant** | No hand-rolled serializer, codec, hash or retry. |
| quality-core YAGNI / KISS / no premature abstraction | **compliant** | Every constant and type earns its place. `BlobTransfers` + `AddAssign` is 25 lines and used on both paths. |
| quality-core DRY | **violation — see F-5** | The phase-2 tag-merge fold is a second implementation of `push_manifest_and_merge_tags`'s. |

---

## 2. Module placement and the seam (question 1)

**The seam is principled, and it is drawn in the right place.**

- `oci/copy.rs` moves **content**: leaf manifest bytes, the blobs the leaf names, the
  referrer chain anchored to it. Everything it writes is a pure add — digest-addressed,
  untagged, invisible until a tag names it. It never touches a tag.
- `publisher/copy.rs` moves **pointers**: which platforms to move, what the target's index
  already holds, the per-platform index merge, the rolling-tag recomputation, the canonical
  tag.

That is exactly the ADR's own three-object distinction (immutable content / mutable
per-platform set / derived pointers) rendered as a module boundary, and the split is
load-bearing rather than cosmetic: the invariant "phase 1 cannot make the target's tags
observably different" is enforced by *which module you are in*, not by a comment. The
module docs at `oci/copy.rs:14-17` state it explicitly.

**Dependency direction is clean in both directions** (question 2):

- `oci/copy.rs` imports only `super::client::*`, `super::{Client, Digest, Identifier}`,
  `super::referrer::*` and `crate::log`. No `package`, no `publisher`, no `package_manager`.
  It knows nothing about cascade, versions, dispositions or requests. Verified by reading
  every `use` and every `super::` path.
- `publisher/copy.rs` calls `Client`-level methods (`merge_platform_into_index`,
  `push_canonical_tag`, `list_tags`, `fetch_manifest_raw_bytes_addressed`) and
  `package::cascade::resolve_cascade_tags`. It never reaches `client.transport()`. Its
  dependency on `package::cascade` mirrors `publisher.rs:209`'s existing edge — precedent,
  not novelty. Its dependency on `crate::cli::UsageError` matches the sanctioned home for
  the exit-code taxonomy (`quality-rust-exit_codes.md`: "define enum in library crate's
  `cli` submodule") and the existing `MetadataResolutionError` in the same file.

No leak in the wrong direction. Nothing in `oci/copy.rs` would have to change if the
publisher's disposition model changed.

### F-1 — The facade is bypassed because the facade bakes in addressing

`oci/copy.rs:249` takes `client.transport()` and drives it directly for `ensure_auth`,
`head_blob`, `mount_blob`, `pull_blob_to_file`, `push_blob_from_path`, `list_referrers`,
`push_referrer_manifest`, `push_manifest_raw`.

Every one of those bypasses is **correct**, and that is the point. `Client::head_blob`
(`oci/client.rs:695-697`) resolves through `transport_reference` — mirror-aware. Calling it
against the *target* of a copy would HEAD the wrong host and conclude a blob was present
when it is not. So the engine had to go around the facade to get canonical addressing.

The facade's read methods each hard-code one addressing choice, so a caller needing the
other choice cannot use the facade at all — it must drop a layer. That is a design smell
in `Client`, not in `oci/copy.rs`, and it directly produced Stage 1's defect: see F-2.

- **Classification:** Actionable [Warn] — but the remediation is F-2's, not a change to
  `oci/copy.rs`.

---

## 3. `ReadAddressing` — is the API shaped backwards? (the posed question)

**Yes, and the evidence is a count rather than a preference.**

`Client::list_tags` (`oci/client.rs:409-411`) is `pub`, short, and defaults to
`ReadAddressing::Mirrored`. `list_tags_addressed` is `pub(crate)`, longer, and requires the
caller to name the addressing. `subsystem-oci.md` invariant #5 states the rule as a
*discipline* — "any read whose answer decides, gates, or verifies a write must ask for
`ReadAddressing::Canonical`" — with nothing in the type system holding it.

Census of production call sites of the write-backing case:

| Call site | Addressing | Correct? |
|---|---|---|
| `package/cascade/gather.rs:69` | `list_tags_addressed(Canonical)` | yes |
| `publisher/copy.rs:285` | `list_tags` (Mirrored default) | **no** |

Two write-backing reads exist in the codebase. One is right, one is wrong. The invariant is
at 50% adherence among exactly the reads it governs, and the wrong one is the one that used
the shorter default-bearing name.

The asymmetry of consequence settles the direction. Choosing `Mirrored` where `Canonical`
was needed is a silent CWE-345/367 defect that no test catches without a mirror fixture.
Choosing `Canonical` where `Mirrored` would do costs a mirror bypass — a performance and
egress annoyance, loudly visible in traffic. A default should sit on the side whose failure
is cheap; this one sits on the side whose failure is a security defect.

**Cost to invert, measured:** `Client::list_tags` has exactly **four** production call
sites — `publisher.rs:259`, `announce/pipeline.rs:224`, `oci/index/oci_index.rs:52`,
`publisher/copy.rs:285`. (The many `list_tags` hits under `oci/index/**` and in
`ocx_cli/src/command/index_*.rs` are `Index`/`IndexImpl::list_tags`, a different method.)
Deleting the defaulted wrapper and making `addressing` a required parameter is a four-line
change plus two test-call updates. That is cheap enough that the frequency argument for a
default does not carry.

The same argument applies to `Client::head_blob`, which F-1 shows the copy engine already
had to route around.

- **Classification:** Actionable [Warn] — `crates/ocx_lib/src/oci/client.rs:409-411`.
- **Remediation:** delete `Client::list_tags`, rename `list_tags_addressed` → `list_tags`
  with a required `ReadAddressing` argument, and update the four call sites. Consider the
  same treatment for `head_blob`. This is the only structural change that would have
  prevented Stage 1's finding rather than relying on a reviewer noticing it.

---

## 4. The write-order question — was the ADR's phase model sound?

**The ADR's phase model was sound. The code's phase-2 placement is an artifact of an API
signature, not of the domain.**

The ADR (`adr_package_copy.md`, "Write order") puts the `sha256.<hex>` canonical tag in
phase 1, among the pure adds. The code writes it in phase 2 (`publisher/copy.rs:250-258`),
and the reason given is that the leaf digest must come from the merged index.

That reason does not survive reading `push_canonical_tag` (`oci/client.rs:642-682`). The
function does three things: (a) look the platform's leaf digest up in the merged index
(line 648), (b) **pull that leaf's manifest bytes back from the registry** (line 669),
(c) push those bytes under the `sha256.<hex>` tag (line 677).

In a copy, both inputs to (a) and (b) are already in hand before any merge runs:
`source_digest` is the loop variable at `publisher/copy.rs:185`, and `copy_leaf` has
already pushed the leaf bytes to the target. The index lookup and the manifest re-pull are
both redundant on this path. The write is in phase 2 because
`push_canonical_tag`'s parameter list demands a merged index — nothing else forces it.

Three consequences, all avoidable:

1. The ADR and the code disagree on a documented write order, and the code's version is the
   weaker one.
2. A crash between the index merge and the canonical tag leaves the target's tag pointing
   at a leaf with no `sha256.<hex>` safety net — the exact state
   `adr_index_indirection.md` Decision E exists to prevent. Re-runnable, so not a blocker,
   but the ADR's own consequence ("`cascade check` passes after a promotion without a
   repair step") does not hold after a partial phase 2.
3. The `primary_index` plumbing at `publisher/copy.rs:240` and `246-249` exists solely to
   feed this call and would disappear.

- **Classification:** Actionable [Warn] — `crates/ocx_lib/src/publisher/copy.rs:250-258`.
- **Remediation:** add `Client::push_canonical_tag_for_digest(identifier, digest, bytes)`
  taking the leaf digest and bytes directly, have the existing index-taking form delegate
  to it after its lookup+pull, and call the new form from `oci/copy.rs` inside phase 1,
  right after the leaf `push_manifest_raw`. The `primary_index` variable and one registry
  round-trip per platform both go away, and the code matches the ADR.

### F-2 — `LeafCopy.size` is dead, and phase 2 re-fetches every leaf manifest to recompute it

`copy_leaf` computes and returns the leaf manifest's byte length (`oci/copy.rs:184-192`).
Its doc comment states the purpose verbatim: *"The leaf manifest's size in bytes, for the
index entry's descriptor"* (`oci/copy.rs:86-87`), and the sibling `digest` field is
documented as *"returned so the caller can merge it into the target's index without
re-reading it"* (`oci/copy.rs:83-85`).

The only production caller drops both. `publisher/copy.rs:204-207` binds `copied` and uses
`copied.blobs` and `copied.referrers` only. Phase 2 then calls
`leaf_size(client, request.source, digest)` at line 227, which issues a **full manifest GET
against the source registry** (`publisher/copy.rs:291-299`) purely to recover
`bytes.len()` — a value phase 1 already had, for the same digest, moments earlier.

Cost: one redundant source-registry manifest fetch per platform per copy. On a five-platform
promotion that is five avoidable round-trips against the registry the ADR's decision drivers
name as a trust boundary. Both `LeafCopy` fields are unreferenced outside `oci/copy.rs`'s
own unit tests.

- **Classification:** Actionable [Warn] — `crates/ocx_lib/src/publisher/copy.rs:204-207`
  and `:227`.
- **Remediation:** collect `(platform, digest, copied.size)` in the phase-1 loop and read
  the size from that in phase 2. Then delete the `leaf_size` helper
  (`publisher/copy.rs:290-299`) — it has no other caller.

### F-3 — `read_target_entries` swallows every error, and the per-platform report then lies

`publisher/copy.rs:376-385` maps *any* read failure of the target's index to "the target has
nothing":

```rust
Err(error) => {
    log::debug!("Target {target} has no readable index yet ({error})");
    return Ok(entries);
}
```

That arm catches a 401, a 5xx, a timeout and a malformed index alike.

The **data** is safe: `merge_platform_into_index` does its own canonical read and
retain-then-insert (`oci/client.rs:504-540`), so the merge preserves target platforms
regardless. The **report** is not. With an empty `entries`:

- every source platform reports `Added` when it in fact `Replaced` a different digest;
- the `KeptNotInSource` loop at `publisher/copy.rs:212-220` emits nothing.

The second one matters. ADR D2 introduces `kept (not in source)` for one stated reason: *"a
platform present in the target but absent from the source is kept, and reported as `kept
(not in source)` so a filtered copy cannot silently leave a mixed index."* A transient 503
on the target's index read turns that guarantee off, and the operator sees a clean report of
a mixed index — indistinguishable from a complete promotion.

This is also `quality-rust.md` ERR-19 / `api-and-idioms.md` IDIOM-04: a discarded `Result`
whose justification comment ("a target that cannot be read yet is a target that has nothing
to preserve") is true for `ManifestNotFound` and false for every other error in the arm.

- **Classification:** Actionable [Warn] — `crates/ocx_lib/src/publisher/copy.rs:381-384`.
- **Remediation:** narrow the swallow to the genuine not-found case
  (`ClientError::ManifestNotFound`) and propagate everything else. A copy that cannot read
  its own target's index should say so before it starts writing, not report a fiction
  afterwards.

### F-4 — `CopyError`'s two-arm shape flattens the `--json` envelope, and leaves an ADR plan item undone

`CopyError` (`publisher/copy.rs:34-59`) has two `#[error(transparent)]` arms — `Usage` and
`Other` — with a hand-written `ClassifyExitCode`. For exit codes this works: `classify`
delegates to each arm, and `cli/classify.rs:158` downcasts it. The unit test at
`publisher/copy.rs:680-708` proves 64 vs not-64.

What it flattens is the structured JSON error envelope. `error_envelope.rs:234-253`
(`collect_context`) and `:264-277` (`collect_detail`) are still hardcoded to `SignError` /
`VerifyError`. The ADR anticipated exactly this — implementation plan item 6 reads:

> `error_envelope.rs` `collect_context` / `collect_detail` arms for the new error kind
> (both are hardcoded to `SignError`/`VerifyError` today, so the JSON `detail` slug would
> otherwise be silently empty).

That item was not done. And it *cannot* be done against the current shape: `CopyError`
carries no kind enum for `collect_detail` to read a slug from, and no identifier for
`collect_context` to attach. So `ocx --format json package copy` emits an envelope with
`detail: null` and `context: {}` for every failure, and the shape offers nothing to fix
that with.

This is the project's documented three-layer error pattern (`arch-principles.md`: `Error →
PackageError → PackageErrorKind`, *"per-package diagnosis in batch ops"*) not being applied
to a subsystem that has an obvious per-object context (source + target identifiers) and an
obvious kind set (`SourceNotFound`, `IndexNamedByDigest`, `PlatformRequired`,
`PlatformAmbiguous`, `NoMatchingPlatform`, `ReferrersUnsupported`). `SignError` /
`VerifyError` are the in-tree exemplars and both are three-layer.

Not a blocker: the envelope still carries `kind`, `message` and `exit_code`, which is what
every non-sign/verify command emits today, so `copy` is no worse than its peers. It is worse
than its own ADR promised.

- **Classification:** Actionable [Warn] — `crates/ocx_lib/src/publisher/copy.rs:34-59`.
- **Remediation:** either (a) give `CopyError` the three-layer shape — a `CopyError {
  source, target, kind }` struct over a `CopyErrorKind` enum with `kind_detail()`, plus the
  two `error_envelope.rs` arms; or (b) record in the ADR that the envelope's `detail` and
  `context` are deliberately empty for `copy`, and strike plan item 6. Silently leaving
  item 6 unchecked is the one outcome to avoid.

---

## 5. Duplication against the existing push path (question 3)

**Verdict: the cascade *algebra* is genuinely shared. The tag-merge *fold* was
re-implemented.**

Shared, verified by reading the call sites — not re-implemented:

| Component | Push path | Copy path |
|---|---|---|
| Cascade tag resolution | `cascade.rs:273` `resolve_cascade_tags` | `publisher/copy.rs:286` — same function |
| Version parsing | `publisher.rs:264` `parse_versions` | `publisher/copy.rs:285` — same function |
| Per-platform index upsert | `client.rs:1243` `merge_platform_into_index` | `publisher/copy.rs:237` — same function |
| Canonical tag | `cascade.rs:287` `push_canonical_tag` | `publisher/copy.rs:252` — same function |

The plan's claim that copy reuses the cascade math is accurate. Nothing about
platform-aware blocker checking, version decomposition or index upsert semantics was
duplicated.

### F-5 — The primary-then-cascade merge fold exists twice

`oci/client.rs:1241-1264` (inside `push_manifest_and_merge_tags`):

```rust
let primary_tag = package_info.identifier.tag_or_latest().to_string();
let (index_digest, index) = self.merge_platform_into_index(&package_info.identifier, &primary_tag, ...).await?;
for tag in extra_tags {
    self.merge_platform_into_index(&package_info.identifier, tag.clone(), ...).await?;
}
```

`publisher/copy.rs:235-249`:

```rust
for tag in std::iter::once(request.target.tag_or_latest().to_string()).chain(tags) {
    let (_, merged) = client.merge_platform_into_index(request.target, tag.clone(), ...).await?;
    if primary_index.is_none() { primary_index = Some(merged); }
}
```

Same six arguments, same primary-first-then-extras ordering, same "keep the primary's merged
index" result. About twelve lines.

Applying the project's own DRY test (`quality-core.md`: extract when *"2+ genuinely
different callers"* exist; incidental similarity is not duplication) — there are now exactly
two, and the similarity is not incidental. The ordering is a documented **partial-failure
safety** property (`cascade.rs:249-250`: *"most-specific → least-specific for partial-failure
safety"*), and a safety-ordering property implemented twice diverges on the first bug fix in
one of them, silently.

Honest counterweight: twelve lines, and `push_manifest_and_merge_tags` also pushes the
manifest and returns `LayerCounts`, so the extraction is not free of shaping work.

- **Classification:** Actionable [Warn] — `crates/ocx_lib/src/publisher/copy.rs:235-249`.
- **Remediation:** extract `Client::merge_platform_into_tags(identifier, primary_tag,
  extra_tags, platform, digest, size, annotations) -> Result<(Digest, ImageIndex)>` carrying
  the ordering and the primary-index capture. `push_manifest_and_merge_tags` calls it after
  its manifest push; copy calls it directly. Ordering then lives in one place with its
  rationale comment.

---

## 6. `push_blob_from_path` as a defaulted trait method (question 4)

**The default buffers, and that defeats the method's entire purpose.**
`oci/client/transport.rs:207-213`:

```rust
) -> Result<String> {
    let data = tokio::fs::read(path).await.map_err(...)?;
    self.push_blob(image, data, digest, on_progress).await
}
```

`tokio::fs::read` loads the whole file into a `Vec<u8>`. For a 200 MB toolchain layer with
four concurrent transfers (`MAX_CONCURRENT_BLOB_TRANSFERS = 4`), a transport that does not
override this allocates up to 800 MB — precisely the condition the ADR's own risk section
and `package-manager-domain.md` PKG-04 name as the reason the method exists.

The doc comment is honest about it (*"Reads the whole file and delegates to
`push_blob`… `NativeTransport` overrides it to stream from the file, which is the entire
point"*), which makes this a documented trap rather than a hidden one.

Today the exposure is nil: `NativeTransport` overrides (`native_transport.rs:440`), and
`StubTransport` is a test double where memory does not matter. The problem is the shape.
A defaulted method whose default is semantically wrong for the method's stated purpose gives
every future implementor a **compiling, plausible, wrong** implementation with no signal.
`ARCH-07`'s sibling concern applies: the default exists for the convenience of one test
double, and it prices that convenience in a latent correctness trap for every production
transport that follows.

Note the contrast with the neighbouring `mount_blob` default (`transport.rs:225-233`), which
returns `UploadRequired` — a default that is *semantically correct* for a transport with no
mounting, degrading gracefully. That is what a good trait default looks like. This one is
not that.

- **Classification:** Actionable [Warn] — `crates/ocx_lib/src/oci/client/transport.rs:201-213`.
- **Remediation:** make `push_blob_from_path` a required method and give `StubTransport` the
  four-line read-and-delegate body directly. The buffering then lives where buffering is
  fine, and a future transport gets a compile error instead of an OOM.

---

## 7. ARCH-01 / ARCH-03 / ARCH-07 / ARCH-12 (question 5)

Run against both new modules.

**ARCH-01 (repeated leading parameter tuple wants a type).** Tripped in `oci/copy.rs`:
`copy_leaf`, `copy_blobs`, `copy_blob`, `copy_referrers` all lead with
`(client: &Client, source: &Identifier, target: &Identifier)`, and three of them also thread
`scratch: &Path`. Five functions lead with `client: &Client`.

I am **not** reporting this as a tidiness finding — `arch-principles.md` and `quality-core.md`
both forbid abstraction for its own sake, and a `CopyEngine` struct would be pure churn if
the only argument were the rule's letter. The argument is specific: `source` and `target` are
**two adjacent parameters of the same type**, `&Identifier`, threaded through four functions.
A transposition compiles silently and copies backwards — writing production content into the
staging registry. All four call sites are correct today (verified: `oci/copy.rs:148`, `:250-251`,
`:341-342`, `:376`), so this is a standing hazard, not a bug.

`publisher/copy.rs` shares only `client: &Client` as a leading parameter; the second
parameter differs per function, so no same-type adjacency hazard exists there.

- **Classification:** Actionable [Suggest] — `crates/ocx_lib/src/oci/copy.rs:113,222,242,326`.
- **Remediation:** a private `struct Transfer<'a> { client: &'a Client, source: &'a Identifier,
  target: &'a Identifier, scratch: &'a Path }` constructed once in `copy_leaf`, with the
  four functions as `&self` methods. Roughly fifteen lines of churn; makes the transposition
  unrepresentable. If the owner judges the hazard theoretical, decline it and say so — this
  is a Suggest, not a Warn.

**ARCH-03 (impl sprawl).** Clean. `oci/copy.rs` adds one small inherent `impl BlobTransfers`
(one method) plus `AddAssign`. `publisher/copy.rs` adds one method to `impl Publisher`
(`copy`) and one `impl CopyOutcome` (one method). Nothing approaches the 2-block / 25-method
ceiling.

**ARCH-07 (single-implementation traits).** Clean. No new trait. The new trait *method* is
covered by F-6.

**ARCH-12 (decision logic mixed with I/O).** Two mild hits, one of which is F-3 in a
different costume:

- `resolve_source_leaves` (`publisher/copy.rs:302-354`) fetches, then decides platform
  filtering and source-form legality in the same body. The pure part is small and the
  decisions are trivially readable; not worth splitting.
- `read_target_entries` mixes an I/O failure *policy decision* with the I/O — that is F-3,
  reported there.

The disposition decision itself (`publisher/copy.rs:186-190`) is already a pure fold over
values with the lookup extracted to a pure `lookup` (`:356-361`). That is the right shape.

---

## 8. Is `copy` the right feature boundary? (question 6)

**Agreed with the owner's decision. No disagreement to register.**

I checked this against `product-context.md` rather than accepting the ADR's own framing.
Three things line up:

1. **Target users.** Primary is *"Automation tools — GitHub Actions, Bazel rules,
   devcontainer features, CI scripts"*; use case 6 is *"Internal tool distribution"* and
   differentiator #7 is *"Private distribution first-class"*. A dev → staging → prod
   promotion pipeline is squarely in that space, not an end-user feature.
2. **Ecosystem vocabulary.** `copy` is the verb every comparable tool uses (`oras cp`,
   `crane copy`, `skopeo copy`, `regctl image copy`). `product-context.md`'s competitive
   matrix claims *"Learning curve: Low"* as a differentiator; inventing a verb would spend
   that.
3. **The `push --from` alternative fails on a real constraint, not a stylistic one.**
   `push` *builds* the leaf manifest — `push_manifest_and_merge_tags` calls
   `push_multi_layer_manifest` (`client.rs:1236`) — and a copy must never rebuild it,
   because the leaf digest is what a signature subjects and a lock pins. Bolting a mode flag
   onto `push` would leave `-f/--file`, `--metadata` and `--build-timestamp` conditionally
   invalid, with `--build-timestamp` — which renames the artifact — one typo from a silent
   mis-promotion. The ADR's Option 2 analysis is correct on this point.

Tier placement is also right: `subsystem-cli-commands.md` files `package copy` under
"Low-level registry", and the implementation consults no `ocx.toml` at any point (verified
by reading `command/package_copy.rs` end to end).

---

## 9. Smaller notes

**N-1 [Suggest] — stray bare block.** `oci/copy.rs:335` opens a bare `{` that wraps the
entire body of `copy_referrers` and closes at `:397`. It scopes nothing — no guard, no
borrow — and reads as though a lifetime once mattered there. Leftover from an earlier shape;
delete the braces and outdent.

**N-2 — two-layer source-form validation is intentional, not duplication.** The CLI checks
the digest-source rules at `command/package_copy.rs:100-118` (syntactic, before any network),
and `resolve_source_leaves` re-checks at `publisher/copy.rs:342-352` (semantic, after
learning whether the fetched manifest is an index or an image). The lib arm stays reachable
for `repo:tag@sha256:<leaf>`, where the CLI's `source.tag().is_none()` guard does not fire.
Both layers are earning their place. Not a finding.

---

## Deferred

**D-1 [Warn] — `ensure_auth(&target)` runs before source-form validation.**
`command/package_copy.rs:123` authenticates against the target registry before
`publisher.copy(...)` runs `resolve_source_leaves`. For the `repo@sha256:<index-digest>`
source form — refused at `publisher/copy.rs:317-321` — the target has therefore already been
contacted by the time the exit-64 refusal fires.

The ADR's Validation checklist promises: *"Every source-form violation exits 64 with the
target registry provably never contacted."* That promise is unachievable for this form from
the CLI, because an index digest and a leaf digest are syntactically identical — you cannot
know which you have without fetching the source.

The tension is genuine and unresolved by the ADR. `Publisher::ensure_auth`'s own doc
(`publisher.rs:103-106`) states the opposing principle: *"Call at the start of a publishing
command to fail fast on credential issues before reading files or doing any other
preparation."* Moving source resolution ahead of `ensure_auth` honours the ADR's criterion
and costs the fail-fast-on-credentials property; leaving it costs the ADR's criterion for
one source form.

**Why this needs a human:** it is a trade between two stated design principles that the ADR
asserted without noticing they collide, and the answer depends on whether "provably never
contacted" was meant as "no write" (already true) or literally "no request" (not true). The
owner should pick one and amend the ADR's Validation line to match.

---

## Summary of findings

| # | Severity | Location | Issue |
|---|---|---|---|
| F-1 | Warn | `oci/copy.rs:249` | Facade bypassed for seven ops — correct individually, symptom of F-2 |
| F-2 | Warn | `oci/client.rs:409-411` | `ReadAddressing` default sits on the unsafe side; 4 call sites, cheap to invert |
| — | Warn | `publisher/copy.rs:250-258` | Canonical tag in phase 2 is an API-signature artifact, not a domain constraint |
| F-2b | Warn | `publisher/copy.rs:204-207,227` | `LeafCopy.size` dead; phase 2 re-fetches every leaf manifest |
| F-3 | Warn | `publisher/copy.rs:381-384` | Blanket error swallow makes the per-platform report lie |
| F-4 | Warn | `publisher/copy.rs:34-59` | `CopyError` shape leaves `--json` `detail`/`context` unfillable; ADR item 6 undone |
| F-5 | Warn | `publisher/copy.rs:235-249` | Partial-failure merge ordering implemented twice |
| F-6 | Warn | `transport.rs:201-213` | Defaulted `push_blob_from_path` buffers — wrong-but-compiling for future transports |
| A-1 | Suggest | `oci/copy.rs:113,222,242,326` | Adjacent same-type `source`/`target` params across four functions |
| N-1 | Suggest | `oci/copy.rs:335,397` | Stray bare block scoping nothing |
| D-1 | Warn (deferred) | `command/package_copy.rs:123` | Target auth precedes source validation; ADR criterion vs fail-fast collide |

No blockers. The seam is right, the dependency direction is clean, and the cascade algebra
is genuinely reused. Every finding above is a refinement of a sound design.
