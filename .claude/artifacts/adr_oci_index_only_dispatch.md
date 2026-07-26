# ADR: The Index Stores OCI Image Indices — Deleting an Invented Object

## Metadata

**Status:** Accepted — owner-ratified 2026-07-25.
**Date:** 2026-07-25
**Deciders:** mherwig
**Beads Issue:** N/A
**Related issues:** [ocx-sh/index#57](https://github.com/ocx-sh/index/pull/57) (first announce PR — the last
moment before published CAS objects exist), [#215](https://github.com/ocx-sh/ocx/issues/215)
**Tech Strategy Alignment:**
- [x] Follows Golden Path in `.claude/rules/product-tech-strategy.md` (no new dependency; net deletion in
  both repos)
**Domain Tags:** oci, index, wire-format, security, announce
**Supersedes:** clauses of [`adr_index_indirection.md`](./adr_index_indirection.md) — A2, A3, C1, D, F1,
F4 (enumerated per clause in D6). The rest of that ADR stands.
**Superseded By:** N/A
**Depends on:** [`adr_index_indirection.md`](./adr_index_indirection.md) (the wire grammar, the
dispatch-only object store), [`adr_platform_model_unification.md`](./adr_platform_model_unification.md)
(`is_compatible` / `select_best`, the lock unit)

---

## Context

### What an index is

An index is a **catalog of OCI artifacts**. That is the whole of it.

OCI tags are floating pointers by definition — a tag is a name a registry may repoint at any time. The
index's entire job is to **lock one**: to record what a floating pointer resolved to at a specific point
in time, so a version choice made later resolves to the same artifact.

It follows that the index has no business defining object shapes of its own. Adherence to OCI here is a
separation-of-concerns property, not a convenience: shape definition belongs to the OCI image spec, and an
index that invents its own shapes has taken on a responsibility that is not its.

Measured against that, `o/` is plain content-addressable storage holding **the OCI image indices the
observed tags referenced**, byte-for-byte as the registry served them. `tags[].content` is the digest of
one of them. There is no third concept.

### Why an index is copied and a manifest is not

This asymmetry is the cause; everything else in this ADR is downstream of it.

1. **A manifest is one package for one platform.** Its content cannot change without it being a different
   package, so a manifest digest is stable *by identity* and a later fetch by digest is hermetic.
2. **A manifest can still disappear** — garbage-collected once *nothing* references it. Liveness is a
   registry-side property with several sources: any tag pointing at it, and equally any image index that
   still lists it. A manifest referenced by an older index that is itself still referenced stays online
   with no tag of its own. Keeping manifests reachable is the registry operator's and the publisher's
   concern, not something OCX owns or can observe.

   Canonical tags are a **tool OCX offers**, not the mechanism liveness depends on. `ocx package push`
   writes a `sha256.<hex>` tag named after each pushed platform manifest's own digest, default on:
   `CanonicalTag::enabled()` returns `!self.no_canonical_tag` (`crates/ocx_cli/src/options/canonical_tag.rs:29`),
   threaded through `package_push.rs:178, 212` into `Publisher::push` — "the default from `ocx package push`"
   (`publisher.rs:85-90`, call at `:114-118`; cascade path at `cascade.rs:228-230`). Locked by
   `publisher.rs:376` `canonical_tag_true_pushes_the_sha256_dot_hex_tag` and `:397`
   `canonical_tag_false_skips_the_extra_tag_push`. Its purpose is to cover an error-prone case — a tag
   moved or a patch released such that nothing points at an older manifest any more — alongside the
   documented build-timestamp guidance. A safeguard, offered; never a requirement OCX imposes or checks.
3. **An index has neither property.** It is a *collection over platforms*: adding a platform produces a new
   index with a new digest, and the tag moves to it (`merge_platform_into_index`, `client.rs:315-336`). The
   previous index is then referenced by nothing and becomes GC-eligible. It disappears in the course of
   ordinary, correct publishing — not as an error case.
4. **Therefore: snapshot exactly the thing that can disappear in the ordinary course of publishing, and
   only that.** The index is copied into our CAS. The manifest never is: it is immutable by digest, and
   whether it stays reachable is the registry's and publisher's concern, not ours to snapshot around.

**The division of responsibility, which is the reproducibility contract:** the package maintainer's
minimum obligation is that manifests stay reachable. OCX guarantees the rest by locking the indices.
Nothing else is assumed of the maintainer, and nothing else is checked (OQ1).

### What is there instead today

`p/<ns>/<pkg>/o/<algo>/<hex>.json` currently holds **two** shapes, chosen by the source's configured
provenance:

- a **derived** source (a plain OCI registry) stores the registry's verbatim OCI image index
  (`local_index.rs:400-409`, `local_index.rs:439-441`);
- a **published** source (`index.ocx.sh`) stores a bot-synthesized object of our own invention —
  `{"platforms":[{"platform":{…},"digest":"sha256:…"}]}` (`wire.rs:93-122`).

The second is a noun we invented where OCI already had one. It carries no information the image index does
not: both are a list of `(platform, manifest-digest)` pairs, and the invented one is that list copied out
of `manifests[]` and nothing else.

- **Producer, ocx side.** `manifest_to_observation` (`announce/pipeline.rs:204-238`) iterates
  `index.manifests`, copies `entry.platform` and `entry.digest`, skips platform-less entries
  (`pipeline.rs:220-222`). Nothing else is read.
- **Producer, index-bot side.** `_platforms_from_index` (`bot/src/indexbot/core/observe.py:105-113`)
  performs the identical projection.
- **Consumer, ocx side.** `observation_to_index` (`ocx_index.rs:777-803`) reconstructs an image index from
  those two fields, filling `media_type` with a constant and `size: 0` (`ocx_index.rs:790-791`) — the round
  trip is complete, and lossy in one direction only. The type has exactly one field (`wire.rs:105-109`).
- **Consumers, index side.** `render.py::_catalog_platforms` (`bot/src/indexbot/core/render.py:139-158`)
  reads `entry.platform.os` / `.architecture`; the website's `useObservation.ts:21-23` /
  `PlatformMatrix.vue:36-42` read `platforms[].platform` and `.digest`. No consumer reads any other field,
  because none exists.

Dropped by the invention: `mediaType`, per-descriptor `size`, `artifactType`, `annotations`, `subject`, and
platform-less descriptors.

The invention also leaks into the read path as a second codec: `decode_index_manifest`
(`local_index.rs:866-876`) tries the OCI parse, then falls back to the invented parse; `resolve_dispatch`
takes a `SourceKind` argument to know which to expect (`local_index.rs:622-625`); `AbsentLeaf` recovery is
"source-kind-routed" (`local_index.rs:831-848`).

### The raw bytes and their registry digest already exist at both producers

- ocx: `observe_one_tag` calls `fetch_manifest_raw_bytes` and **discards** the first two elements —
  `let Some((_bytes, _digest, manifest)) = fetched else` (`announce/pipeline.rs:181`).
- bot: `GhcrRegistry.get_manifest` returns `ManifestFetch(raw=raw, digest=computed_digest,
  parsed=response.json())` after cross-checking `Docker-Content-Digest`
  (`bot/src/indexbot/adapters/ghcr.py:217-228`).

Both producers hold the registry's own bytes and the registry's own digest at the moment they choose to
throw them away and synthesize something else.

### Costs of the invention

These are the ongoing costs. The *root* defect — that a projection cannot serve the purpose a snapshot
exists for — is stated in the Decision below, because it is the reason the shape is wrong rather than
merely expensive.

1. **Separation of concerns.** We define a wire object, a canonical serializer for it
   (`bot/CONTRACTS.md:975-997`, ported byte-exactly into Rust at `wire_writer.rs:53-98`), a JSON schema for
   it, a platform sort key in two languages, and a cross-language conformance corpus proving the two agree
   — all to restate a document OCI already specifies and the registry already served.
2. **Security.** The platform→digest mapping is **bot-authored**. An index compromised on its own — no
   registry write — can fabricate a mapping, and every digest still verifies against its own bytes, because
   nothing ties the *mapping* to anything a registry said. This is the mix-and-match threat of
   `adr_index_indirection.md` §Non-Goals in its cheapest form.
3. **Stability.** Our digests are documented as **not stable across re-announces**
   (`adr_index_indirection.md` F4; `subsystem-oci.md` §Gotchas; `website/src/docs/in-depth/indices.md:196-197`)
   because we serialize them and our sort key had a defect. A registry image-index digest is stable by
   construction — it is the artifact's identity.
4. **Copy-paste.** A hosted subtree copied into a local collection is not self-describing: decoding `o/`
   requires knowing the source's configured provenance. One shape makes decode unconditional. This is a
   consequence of (1), not an independent argument.
5. **Cost.** Hop count is unchanged — the saving in `adr_index_indirection.md` C1 came from caching a
   dispatch object at all, not from its shape. One `GET` under `o/<algo>/<hex>.json` either way.

---

## Decision Drivers

- **DR1. The index defines no object shapes.** Shape definition is the OCI image spec's job.
- **DR2. Verifiability over invention** (inherited, `adr_index_indirection.md` DR2). The invented object is
  the last OCX/bot-authored artifact sitting in the resolution path.
- **DR3. Cheapest moment wins.** Migration cost is linear in announced tags and grows permanently.
- **DR4. Pre-1.0 clean break.** No compat shim, no dual-read fallback, no migration code
  (`project_breaking_compat_next_version`, `feedback_refactor_as_if_never_existed`).

---

## Decision

### D1 — `o/` holds OCI image indices, verbatim

**`p/<ns>/<pkg>/o/<algo>/<hex>.json` holds the bytes a registry served for an OCI image index, unmodified.
`<hex>` is `sha256` of those bytes, which is the registry's own manifest digest for that index.** The
invented object is deleted from the format.

A tag's `RootTag.content` is therefore an image-index digest, for every source kind.

The term "observation object" is retired from the vocabulary. The thing is a copy of the registry's image
index at the instant we observed the tag; call it that. (Type names in code are an implementation
follow-up, listed in §What Breaks — not a softening of this.)

#### Why the invented object is wrong at the root, not merely redundant

A snapshot exists to stand in for an artifact that may no longer be fetchable. That is its entire purpose
— and per Context, an index becoming unfetchable is the ordinary case, not a failure.

It follows that the snapshot must be **the bytes**. A re-serialized projection can never be verified as a
faithful copy of what it replaced, precisely because the original is gone by the time you would want to
check. **A snapshot you cannot verify against the thing it replaces is not a snapshot; it is an
assertion.** The invented object is an assertion standing in the one position where the design requires a
copy.

This is the existing principle carried to its conclusion, not a reversal of it.
`adr_index_indirection.md` Decision D already exempts the `content` pointer from the
platform-manifest-only lock doctrine on exactly this ground — the bytes travel *with* the pointer, so
"there is no later, re-resolvable fetch to tamper with", or as `subsystem-oci.md` puts it:

> its bytes travel *with* the pointer in the same `o/` — no later re-resolvable fetch exists for the
> doctrine to protect against.

That clause is this same asymmetry stated from the other side. What it did not notice is that the claim
holds only if the bytes in `o/` *are* the artifact. Under a projection, there is still no later
re-resolvable fetch — and now no way to check the stand-in either.

The lossy-projection observation (§Context) is a corollary: the projection drops fields. The defect is
prior to that, and would stand even if the projection were lossless.

### D2 — Indices only. Enforced. No exceptions.

**If a tag resolves to an image manifest rather than an image index, we do not record it — and this is
checked, not assumed.** No dual mode, no absence rule, no fallback shape.

This deletes the disambiguation-by-absence rule of `adr_index_indirection.md` A3 ("`content` **absent**
from `o/` → by construction it names a **manifest**") for tag entries. `content` is never absent: every tag
entry names an index that is present.

**Scope boundary, stated because it will be misread.** The rule governs **tag entries in a root**.
Digest-addressed resolution — a pinned `ocx.lock` leaf, a `pkg@sha256:…` reference — is content addressing,
not index dispatch, and is untouched: leaf platform manifests are still fetched by digest and still never
stored in `o/` (`adr_index_indirection.md` A3/B2, unchanged).

### D3 — The resulting invariant

> Every tag in the index points at an OCI image index; that index is present in `o/`; and its bytes are
> byte-identical to what the registry served. Every digest the index locks is a digest a registry serves,
> and no bot-authored content sits in the resolution path.

No exceptions, no second shape, no absent case.

### D4 — Where the check lives

Three layers, each with a distinct job. The first is the amendment's "refuse and report, do not silently
skip"; the second keeps the rule from firing on tags that were never version pointers; the third makes the
invariant checkable without a registry.

**(a) Announce / observe, per curated tag — refusal.** A tag the publisher explicitly curated whose
manifest is not an image index is a hard error naming the tag and the repository. ocx already raises
exactly this today: `AnnounceError::SinglePlatformManifest` (`announce/pipeline.rs:209-217`,
`announce/error.rs`), fired when `manifest_to_observation` receives an `oci::Manifest::Image`. Rename it to
`TagIsNotAnImageIndex` — the current name describes a platform count, and the rule is about document kind.
The bot's `observe_one_tag` (`observe.py:147-188`) gains the mirror-image refusal, replacing its
`"manifests" in raw` branch (`observe.py:177-181`). What the publisher sees:

```
error: tag '1.2.3' on 'oci://ghcr.io/acme/widget' resolves to an OCI image manifest, not an image index
       the index records image indices only; `ocx package push` always publishes one, so this artifact
       was not published by ocx
```

**(b) The sweep's universe — exclusion, not refusal.** `observe()` walks `registry.list_tags()`
(`observe.py:191-209`), which returns every tag in the repository, including tags that are not version
pointers at all. Those are excluded from the universe before the rule applies — the bot already does this
for one of them (`_DESC_TAG`, `observe.py:25-32`). Two exclusions are required:

- `__ocx.desc` — the description artifact (`package/tag.rs:28`). Already excluded.
- `sha256.<hex>` **canonical tags** — pushed by default (`adr_index_indirection.md` Decision E) and
  pointing at a bare platform manifest by construction: `push_canonical_tag` pulls the platform manifest and
  re-pushes it under the digest-named tag with `MEDIA_TYPE_OCI_IMAGE_MANIFEST` (`client.rs:387-398`).
  **Not currently excluded.** See R3 — without this the rule breaks on every ocx-published repository.

Exclusion is not a fallback shape: these tags are not version pointers, and the sweep's universe is "tags
that could be a version". A publisher who *explicitly* curates `sha256.<hex>` is not refused either: the
tag is not a version, so there is nothing for (a) to rule on. `resolve_curated_tags` partitions the
reserved names out of the resolved selection under D7 (`announce/pipeline.rs:139-140`); what happens next
depends on what survives the partition, and the two outcomes differ in exit code.

- **Mixed selection** — at least one version survives alongside the reserved names. The drops ride out on
  the outcome, the CLI names every one of them on stderr, and the run exits `0`
  (`crates/ocx_cli/src/command/package_announce.rs:164-171`).
- **Wholly reserved** — nothing survives. There is no outcome for that notice to ride on, so the names are
  carried in `AnnounceError::NoCuratedTags { reserved_dropped }` instead (`announce/pipeline.rs:141-143`),
  which classifies as `ExitCode::UsageError` — **64** (`announce/error.rs:191`) — and the names surface in
  the error text, not a stderr notice. Pinned by
  `all_reserved_selection_names_the_dropped_tags_and_exits_usage_error` (`announce/error.rs:259`). This is
  still not a refusal *of the tag*: the invocation named no version at all, which is a usage fault.

Refusal by (a) is for a curated tag that *is* a version pointer and resolves to the wrong document kind.

**(c) CI validation over committed roots — recommended.** `cli/validate.py`'s per-PR gate asserts, for
every `tags[].content`: a CAS object exists at `o/<algo>/<hex>.json`, its bytes hash to that digest, and
they parse as an OCI image index. This replaces what `check_no_dangling_references`
(`validate_entry.py:372-391`) did for tags, upgrades it from presence to shape, and closes the loop on
hand-edited PRs with no registry access required. One parse per object.

### D5 — The rule rejects nothing `ocx package push` produces

Verified in `crates/ocx_lib/src/oci/client.rs::merge_platform_into_index`:

- no existing manifest → start a fresh empty `ImageIndex` (`client.rs:302-311`);
- an existing **bare** `Manifest::Image` → **wrap it** into an `ImageIndex` as a descriptor rather than push
  alongside it (`client.rs:276-298`);
- either way, add the platform entry (`client.rs:315-323`) and push with `MEDIA_TYPE_OCI_IMAGE_INDEX`
  (`client.rs:332-336`).

The merged index describes itself as an ocx package index at one decision point after the match: the
`artifact_type` field is filled with `MEDIA_TYPE_PACKAGE_V1` — `application/vnd.sh.ocx.package.v1`
(`media_type.rs:11`) — when absent and a declared foreign value is left alone (`client.rs:335-341`).
Filling an absent field states what we wrote; overwriting a declared one would relabel someone else's
artifact.

So every package-content tag `ocx package push` writes is an image index. The indices-only rule refuses
nothing OCX produces; it refuses artifacts that were never OCX packages. The two ocx-written tags that are
*not* image indices — `__ocx.desc` and `sha256.<hex>` — are not version pointers and are handled by D4(b)
exclusion, not by weakening the rule.

**The rule is over-determined, and the second reason is the stronger one.** Beyond "nothing OCX-published
is refused": a bare manifest *needs no snapshot at all*. It cannot change — stable by identity (Context,
point 1) — so unlike an index it does not get superseded in the ordinary course of publishing. There is
nothing for `o/` to preserve and nothing a snapshot could add; a copy would be a second copy of bytes
still addressable by the same digest. So a tag pointing at a bare manifest is not merely out of scope —
it is a case where the index has no work to do. Refusing it is not a narrowing of the catalog; it is
declining to record something that would carry no information.

The one narrowing with a real cost is on **derived** indices: a foreign single-platform repository indexed
from a plain OCI registry has tags pointing at bare manifests. Those tags can no longer be locked into a
derived index. They still resolve live (Default mode falls through to the source on a local miss); they
become unresolvable under `--offline`. This is the price of an invariant with no exceptions, and it is
stated rather than carved around — ocx-published repositories are unaffected.

### D6 — Supersession of `adr_index_indirection.md`, clause by clause

| Location | Superseded clause | Replacement |
|---|---|---|
| **A2** (published-index bullet) | "an `rsync` / `wget --mirror` … it is **self-verifying** with no OCX-minted metadata" — true of roots, false of `o/` under two codecs | Holds unconditionally: an `o/` object verifies against its filename *and* against the registry |
| **A3** headline | "`p/<repo>/o/` holds *dispatch objects* only — **observation objects (published indices)** and image indexes (derived indices)" | "…holds OCI image indices only, verbatim" |
| **A3** step 1 | "decode it (obs object for a published index, image index for a derived index)" | "decode it as an OCI image index" |
| **A3** step 2 + "Absence disambiguates itself" | "`content` **absent** from `o/` → by construction it names a **manifest**" — the entire absence-disambiguation rule **for tag entries** | Deleted (D2). A tag's `content` is never absent. The decode-and-self-heal fallback survives only for digest-addressed identifiers, as content addressing, not dispatch |
| **A3** last paragraph | "For a published index the obs object always travels with the copy, so absence there is a damaged copy that re-fetches the obs object from the index site" | Absence is a damaged copy in **both** kinds, recoverable from the index site **or** the physical registry, identically |
| **C1** pipeline step 2 | "`tags[tag].content` (obs digest) → GET `o/sha256/<obs-digest>.json` → `platforms[]` : `select_best`" | "`tags[tag].content` (image-index digest) → GET `o/<algo>/<hex>.json` → `manifests[]` : `select_best`" |
| **C1** bullet | "The image-index hop that a normal OCI resolve performs … is skipped entirely — the obs object already carries the per-platform list" | The image-index hop is **served by the index instead of the registry**. Not skipped — relocated. Hop count identical |
| **D** Corollary | "the local index records as its `content` the **image-index digest** (derived index) or the **observation-object digest** (published index)" | Image-index digest, both kinds |
| **D** exemption rationale | unchanged in force, strengthened | The named bytes still travel with the pointer; they are now *also* independently verifiable against the registry — two anchors instead of one |
| **F1** table row | "`o/sha256/<hex>` **observation object** (●) — immutable (CAS)" | "`o/<algo>/<hex>` **OCI image index** (●) — immutable (CAS)" |
| **F1** trust-anchor sentence | "the obs-object verify step … is the primary place OCX re-derives a digest OCX did not mint" | Still the primary place; the digest is now one a **registry** minted, not one the bot minted |
| **F4** bullets 2–3 | "Obs platform objects are verbatim OCI…"; "**Obs-digest instability caveat (index-bot sort-key bug)**" | Retired. An image-index digest is stable by construction; the sort-key hazard has no surface left |
| **R4** (rejected alternative) | **not** resurrected | R4 rejected storing obs data as a *locally-computed synthetic* image index, because "the synthetic index has no registry-verifiable digest". This stores the registry's own bytes under the registry's own digest — R4's objection is precisely this ADR's objection to the invented object |

A1, A4, B, C2, C3, E, G, H are untouched.

### D7 — Reserved tags are never versions

Two tag classes must never appear as versions in the index:

- **`__ocx*`** — the OCX-internal namespace. **No dot, case-insensitive.** The prefix *is* the namespace;
  reserve all of it. Not widened to bare `__`, which would reserve more than ours.
- **Canonical `sha256.<hex>` digest-alias tags** — a real manifest, but digest-addressed, not a version.

**Today's gap.** `INTERNAL_TAG_PREFIX = "__ocx."` (`package/tag.rs:9`) requires the dot and is
case-sensitive, so bare `__ocx`, `__ocxfoo`, and `__OCX.desc` all fall through as ordinary tags — they
list, and they announce.

**Single source of truth — it already exists.** `Tag::from` (`tag.rs:103-117`) classifies every tag, and
the enum already models "is this a version": `Internal(InternalTag)` (`tag.rs:75`) and `Canonical(Digest)`
(`tag.rs:80`) sit alongside `Version` and `Latest`. The bug is that the filters ask a different, weaker
question — `Index::list_tags` (`oci/index.rs:247-256`) filters on the cheap string check
`Tag::is_internal_str` (`tag.rs:98-100`), which covers only the `__ocx.` half. So:

- widen `INTERNAL_TAG_PREFIX` to `"__ocx"` and match case-insensitively;
- **fix `Canonical` to match the tag form ocx actually writes** — see the note below;
- put the verdict on `Tag` itself (`Internal | Canonical` ⇒ not a version), so classification and policy
  live in one type;
- point `list_tags` and announce curation at that verdict, replacing the string check.

> **Implementation note — `Tag::Canonical` does not currently match a canonical tag.** `Tag::from` reaches
> `Canonical` via `Digest::try_from` (`tag.rs:111`), which requires the **colon** form:
> `strip_prefix(algorithm.prefix()).and_then(|s| s.strip_prefix(':'))` (`oci/digest.rs:223`). The tags
> `push_canonical_tag` writes use a **dot** — `format!("{algorithm}.{hex}")`, because OCI forbids `:` in
> tags (`client.rs:392-394`). So `Tag::from("sha256.abc…")` returns `Tag::Other` today; the enum's
> `Canonical` arm was built for digest *references*, and its test uses the colon form
> (`tag.rs:172-178`). Classifying the dot form is part of this change — without it the new verdict
> compiles, reads correctly, and silently never fires on the exact tags it exists to exclude. This is the
> same defect shape as the missing-dot bug above: a classifier matching a form nobody writes.

**Where the filter applies in announce: once, on the resolved selection.** After `Replace` / `UnionFile` /
`Refresh` collapse to a concrete set (`crates/ocx_cli/src/command/package_announce.rs:100-109`) — not
three times at three sources, which is how the two halves of the internal-tag check drifted apart in the
first place. `--refresh` and `--tags-file` take the base root's committed tags as their starting set
(`announce.rs:292-295`), so they are **carriers, not sources**: neither can introduce a reserved tag, but
either will re-announce one forever if it ever lands. Filtering after resolution covers that for free.

**Three layers, three different verbs.** This is the separation-of-concerns point:

| Layer | Verb | Why |
|---|---|---|
| **push** | **reports** | Facts about what it wrote. Nothing more — push does not decide catalog policy |
| **announce** | **ignores** | Drops reserved tags from any source and emits a notice naming what it dropped. Ignored, not silent; ignored, not refused — refusing would make announce enforce a *usage pattern* rather than apply its own catalog policy |
| **the index** | **rejects** | A committed root containing a reserved tag is invalid |

The index layer is **not** redundant with announce: the bot receives pull requests, and a hand-authored
one never touches ocx. Governance holds at the boundary or it does not hold.

**The push-reporting gap.** `PushOutcome` (`publisher.rs:42-51`) carries only `manifest_digest` and
`cascade_tags`, so the canonical tags push writes are reported nowhere — `push_canonical_tag`
(`publisher.rs:114-118`) returns `()`. They belong in `PushReport` (`crates/ocx_cli/src/api/data/push.rs:21-42`,
built at `package_push.rs:225-229`), the fact channel consumed by humans and by `ocx-mirror pipeline push`
(`push.rs:16-19`). They do **not** belong in `--announce-file` (`package_push.rs:217`): its sole consumer
would discard them under D7, and the file's name would then be a lie.

**Cross-repo truth: pin the contract, since the code cannot be shared.** A **shared fixture list** of tag
names with expected verdicts, committed in the index repo and consumed by both test suites — ocx's Rust
tests and the bot's pytest. Precedent from this session: `test/tests/fake_forge.py` modelled the same
wrong 404 the client did, so 32 acceptance tests certified a defect that broke every first announce. Two
implementations of one rule agree with themselves indefinitely unless something external pins them.

**Current cleanliness is omission, not policy.** `#57` contains no reserved tags because push never
emitted any into a curated set — not because anything filtered them. `ocx package announce --tags
__ocx.desc,sha256.abc…` is accepted today.

---

## What Breaks

### `ocx-sh/ocx` (this repo)

| Call site | Change |
|---|---|
| `crates/ocx_lib/src/oci/index/wire.rs:93-122` | Delete `Observation`, `ObservationPlatform` |
| `crates/ocx_lib/src/oci/index/wire.rs:291-335` | Delete the four wire-pinning tests for them |
| `crates/ocx_lib/src/oci/index/wire_writer.rs:53-98` | Delete `serialize_observation` + `platform_sort_key`; `serialize_root` stays (roots keep the §14 pretty form) |
| `crates/ocx_lib/src/oci/index/wire_writer.rs:315-347, 369-387` | Delete the platform-sort and minified-form tests |
| `crates/ocx_lib/src/oci/index.rs:19-20` | Drop `Observation`, `ObservationPlatform`, `serialize_observation` from the re-exports |
| `crates/ocx_lib/src/oci/platform.rs:142-145` | Add `Platform::candidate_from_descriptor` — the single eligibility rule for an image-index descriptor: no `platform` key, or a platform OCX cannot represent, is not a candidate and is not an error. Both enumerations route through it — `Index::fetch_candidates` (`oci/index.rs:368-383`) for selection and `Platform::from_image_index` (`platform.rs:152-164`) for `ocx index list --platforms` (`crates/ocx_cli/src/command/index_list.rs:164`) — because verbatim bytes carry the attestation and referrer descriptors the projection filtered out (R4) |
| `crates/ocx_lib/src/oci/index/ocx_index.rs:777-803` | Delete `observation_to_index` |
| `crates/ocx_lib/src/oci/index/ocx_index.rs:623-657` | `resolve_observation` → `resolve_index_object`: same fetch, same `sha256` verify, parses `oci::Manifest::ImageIndex` |
| `crates/ocx_lib/src/oci/index/ocx_index.rs:672-687, 902-906, 956-973` | `resolve_tag` / `fetch_manifest` / `fetch_manifest_raw_bytes` return the fetched index directly; synthetic-index construction disappears |
| `crates/ocx_lib/src/oci/index/local_index.rs:866-876` | `decode_index_manifest` collapses to one OCI parse; the second codec and its `Err` arm go |
| `crates/ocx_lib/src/oci/index/local_index.rs:820-848` | `DispatchResolution::AbsentLeaf` loses its source-kind routing **and** its "content names a leaf" meaning for tag entries. It now means only "the index object is missing from `o/`" — rename to `AbsentDispatch`; recovery is unconditional fetch-by-digest |
| `crates/ocx_lib/src/oci/index/local_index.rs:622-625` | `resolve_dispatch`'s `SourceKind` argument survives only for the root/catalog read (`local_index.rs:687-706`), never for decode |
| `crates/ocx_lib/src/oci/index/chained_index.rs:300-303` | The "a published-source AbsentLeaf names an observation-object digest that no registry blob endpoint serves" comment is false; a published absent dispatch now recovers from `$OCX_HOME/blobs` and self-heals into `o/`, exactly as a derived one does |
| `crates/ocx_lib/src/package_manager/tasks/common.rs:485-497` | Delete the `ChainRole::Index` + `physical_reference().is_some()` skip. A published source's `Index`-role chain entry is a genuine registry image index and must be staged like a derived one — which also restores `add_index_retention_edges` parity for index-resolved packages |
| `crates/ocx_lib/src/package_manager/tasks/common.rs:359-370` | Doc-comment: the `ChainRole::Index` published/derived split disappears |
| `crates/ocx_lib/src/package_manager/tasks/common.rs:1123-1198` | The test "the observation-object digest must never be staged into the blob store" inverts: the dispatch digest **is** staged |
| `crates/ocx_lib/src/announce/pipeline.rs:21, 168-197` | `observe_one_tag` stops projecting: keep `(bytes, digest, manifest)` from `fetch_manifest_raw_bytes` (`pipeline.rs:181`) as `Observed { content, bytes }` |
| `crates/ocx_lib/src/announce/pipeline.rs:204-238` | Delete `manifest_to_observation`. Its `Manifest::Image` arm becomes the D4(a) check — keep the guard, drop the projection |
| `crates/ocx_lib/src/announce/error.rs:113` | Delete `EmptyObservation` and `MalformedPlatformDigest` (projection failures that no longer exist); rename `SinglePlatformManifest` → `TagIsNotAnImageIndex` with the D4(a) message |
| `crates/ocx_lib/src/announce/pipeline.rs:556-620` | Delete the four `manifest_to_observation` tests; keep and retarget `rejects_a_single_platform_manifest` as the D4(a) regression test |
| `crates/ocx_lib/src/announce.rs:9-11` | Module doc: "content-addressed observation objects" → "content-addressed OCI image indices" |
| `crates/ocx_lib/tests/serializer_parity.rs` + `tests/fixtures/index_wire/observation/**` | Delete the observation vectors and their parity assertions; root vectors stay |
| `test/scripts/sync_index_conformance.sh` | Drop the observation half of the vendored corpus |
| `test/src/static_index.py:12, 42-46, 80` | `observation_bytes()` emits a real image index; the wire map in the docstring changes |
| `test/tests/test_index_ocx_sh.py`, `test/tests/test_announce.py` | Fixture bytes and asserted CAS filenames change; add a D4(a) refusal case and a D7 drop-with-notice case |
| `crates/ocx_lib/src/package/tag.rs:9` | `INTERNAL_TAG_PREFIX` widens to `"__ocx"`; matching becomes case-insensitive (D7) |
| `crates/ocx_lib/src/package/tag.rs:103-117` | `Tag::from` classifies the `sha256.<hex>` **dot** form as `Canonical` — today only the colon form matches, via `Digest::try_from` (`oci/digest.rs:223`), so real canonical tags land in `Other` |
| `crates/ocx_lib/src/package/tag.rs:88-101` | Add the verdict (`Internal \| Canonical` ⇒ not a version) as a method on `Tag`; `is_internal_str` loses its filter-pipeline callers |
| `crates/ocx_lib/src/package/tag.rs:162-208` | Extend the parsing tests: bare `__ocx`, `__ocxfoo`, `__OCX.desc`, and the `sha256.<hex>` dot form |
| `crates/ocx_lib/src/oci/index.rs:245-256` | `list_tags` filters on the `Tag` verdict, not `is_internal_str`; the doc comment's "prefixed with `__ocx.`" is wrong twice over |
| `crates/ocx_lib/src/announce.rs` (curated resolution) | Apply the D7 filter once on the resolved `TagSelection`, after `package_announce.rs:100-109` collapses it; emit a notice naming every dropped tag |
| `crates/ocx_lib/src/publisher.rs:42-51` | `PushOutcome` gains the canonical tags written (`push_canonical_tag` at `:114-118` currently returns `()`) |
| `crates/ocx_cli/src/api/data/push.rs:21-42` | `PushReport` gains the canonical-tag field; `package_push.rs:225-229` populates it. `--announce-file` (`package_push.rs:217`) is **not** touched |

`select_best` needs **no** change — it already consumes `oci::Manifest::ImageIndex`, which is precisely
why the deleted projection had to be adapted back into one before selection could run.
`Index::fetch_candidates` does change: verbatim
bytes hand it the descriptors the projection used to filter out, so it asks
`Platform::candidate_from_descriptor` per descriptor and skips the ones that are not candidates
(`oci/index.rs:368-383`) — the R4 rule, in the selection half.

### `ocx-sh/index`

| Call site | Change |
|---|---|
| `bot/src/indexbot/model.py:113-146` | Delete `ObservationObject`, `PlatformEntry`, `OciPlatform` |
| `bot/src/indexbot/core/observe.py:105-113` | Delete `_platforms_from_index` |
| `bot/src/indexbot/core/observe.py:116-123` | **Delete** `_platforms_from_bare` — not adapt. It exists only to manufacture a stand-in for the shape D2 refuses. `_resolve_bare_platform` (`:90-102`) and `_parse_platform` (`:68-87`) go with it |
| `bot/src/indexbot/core/observe.py:126-127, 147-188` | Delete `_content_digest`; `observe_one_tag` becomes `fetch = registry.get_manifest(...)` → refuse unless the parsed body is an image index (D4(a)) → `Observation(content_digest=fetch.digest, raw=fetch.raw, …)` |
| `bot/src/indexbot/core/observe.py:25-32, 191-209` | `_DESC_TAG` gains a canonical-tag sibling; `observe()` excludes both from the sweep universe (D4(b), R3) |
| `bot/src/indexbot/core/observe.py:50-65` | `Observation` keeps `tag` / `content_digest` / `source`, replaces `object` with `raw: bytes` |
| `bot/src/indexbot/core/validate_entry.py:540-639` | Delete the `ObservationObject ↔ dict` codec, `platform_sort_key`, `serialize_observation_object`, `parse_observation_object` |
| `bot/src/indexbot/core/validate_entry.py:372-391` | `check_no_dangling_references` is superseded for tags by the D4(c) shape check; keep its desc-blob half |
| `bot/src/indexbot/core/render.py:139-158` | `_catalog_platforms` parses `manifests[]`, and must itself skip platform-less and `unknown/unknown` descriptors — the filtering the projection did upstream |
| `bot/src/indexbot/core/diff.py:22, 52` | `Patch.new_objects` becomes `tuple[tuple[str, bytes], ...]` |
| `bot/src/indexbot/core/verify_claims.py:90-110` | `_verify_tag_claim` compares the claimed `content` against `ManifestFetch.digest` — cheaper, and an equality against a registry-computed value rather than a re-derived projection |
| `bot/src/indexbot/cli/validate.py` | Add the D4(c) gate: every `tags[].content` has an `o/` object that hashes to it and parses as an image index |
| `bot/src/indexbot/core/regenerate.py`, `cli/{announce,reconcile,seed_import,classify_pr}.py`, `core/policy.py` | Consume the retyped record; no semantic change beyond the payload |
| `schema/observation-object.schema.json` | Replace with an image-index schema. **Must not be `additionalProperties: false`** (the current file is, at lines 15, 32, 52): OCX does not author these bytes, and a real index may carry `subject`, `artifactType`, `annotations`, or future spec fields. Validation becomes structural, not closed |
| `schema/root.schema.json:147-150` | `tagEntry.content` — "Digest of the observation object in this index's own package-local CAS — not an OCI manifest or image-index digest" — is now exactly backwards. Rewrite: "Digest of the OCI image index this tag resolved to, as served by the physical registry; those bytes are stored verbatim at `o/<algo>/<hex>.json`" |
| `schema/root.schema.json:61-65` (`tags`) | Add `propertyNames` rejecting the D7 reserved classes — case-insensitive `__ocx*` and `sha256.<hex>` — so a hand-authored PR fails schema validation, not only the bot's semantic checks. JSON Schema has no case-insensitive flag, so the pattern must spell the folding out (or the bot check carries it and the schema documents the intent — an index-repo call) |
| `bot/src/indexbot/core/validate_entry.py` | The D7 index-layer **rejection** for committed roots — the layer announce's ignore cannot cover, since a hand-authored PR never runs ocx |
| `bot/tests/` + `test/tests/` (shared fixture) | Commit the D7 tag-name/verdict fixture list in the index repo; consume it from both suites (D7 "cross-repo truth") |
| `bot/CONTRACTS.md:975-997` (§14), §1, §7 | Delete the observation-object byte-exact form and the platform sort key; §14 keeps the root form only |
| `bot/tests/golden/serializer/observation/**`, `bot/tests/golden/render/**`, `scripts/demo-fixtures/**`, `demo/**` | Regenerate every golden and demo fixture carrying the invented object |
| `site/.vitepress/theme/composables/useObservation.ts:3-23, 44-46` | Interfaces become the image-index shape; the CAS URL is unchanged |
| `site/.vitepress/theme/components/detail/PlatformMatrix.vue:7-10, 36-42` | Read `manifests[]`, same descriptor filter as `_catalog_platforms` |
| `site/src/docs/reference/{wire-format,entry-schema,changelog}.md`, `site/src/docs/explanation/architecture.md`, `site/src/docs/how-to/announce-a-package.md` | Rewrite the observation-object sections |

### Documentation surfaces (ocx repo)

`website/src/docs/in-depth/indices.md:193, 196-197, 211, 214, 227, 232, 242` (the immutability row, the
whole "Cached observation digests are not guaranteed stable" warning block, the tree diagram, the "no
image-index digest to store here" paragraph); `website/src/docs/reference/configuration.md:103, 192, 227`;
`website/src/docs/user-guide.md:719`; `.claude/rules/subsystem-oci.md` ("LocalIndex — wire-grammar
collection, dispatch-only", the component model, the `index.ocx.sh` pipeline block, the obs-digest
stability gotcha); `.claude/rules/arch-principles.md` (Index glossary row, ADR-index row);
`.claude/artifacts/adr_index_indirection.md` (Changelog entry recording this supersession);
`.claude/artifacts/adr_announce_publisher_surface.md` and
`.claude/artifacts/design_spec_announce_initiative.md` (announce register CAS clauses).

Per `feedback_no_migration_prose_in_docs`: user-facing docs describe the new shape only. No "formerly an
observation object" note anywhere on the website.

---

## Migration

**There is none, and that is the decision.** Pre-1.0 breaking changes just break
(`project_breaking_compat_next_version`).

- There is no published `RootTag.content` value to change and no published-source `o/` object to
  re-derive: `index.ocx.sh` has announced zero tags (measured below). The shape lands on an empty
  population; it is not applied to one.
- A derived source already stored the registry's verbatim image index (§Context), so a derived local copy
  is already the shape this ADR names. A local copy is disposable in any case — `ocx index update`
  regenerates one, a user may delete `$OCX_HOME/index/<source>/`, and the local index is outside the GC
  graph (`adr_index_indirection.md` B1), so nothing else is affected.
- No dual-read fallback, no shape probing, no transcoder, no "formerly an observation" comment.
- **D7 needs no migration either.** No committed root contains a reserved tag today — `#57`'s root has
  `"tags": {}`, and push has never emitted one into a curated set. The rule lands as pure prevention, with
  no existing entry to clean up. This too is only true now.

**Cost, measured against the live tree.** `/home/mherwig/dev/index` at `main` holds exactly one package
root — `p/michael-herwig/ocx-e2e-hello.json`, with `"tags": {}` — and **zero** `o/sha256/` objects under
`p/` (every `o/sha256/` path in that repo is a test golden, a demo fixture, or a `.claude/worktrees/` copy).
Track E's 42-package fleet is unannounced. The migration is currently a zero-object rewrite of one root
with an empty tag map, plus code and fixtures. It grows by one CAS object and one committed `content` value
per announced tag, permanently: after Track E the same change is a 42-package all-tags republish plus a
format break for every consumer that already snapshotted. This is the cheapest moment the decision will
ever have.

---

## Consequences

**Positive**

- **The index defines no shapes.** Deleted across both repos: one wire type family, one canonical
  serializer and its byte-exact spec section, one JSON schema, one platform sort key (twice), one
  synthetic-index adapter, one decode fall-through, one source-kind decode routing, one chain-staging
  special case, two projection functions, plus their tests, golden vectors, and cross-language conformance
  corpus.
- **The mapping is registry-attested.** An index-only compromise can no longer fabricate a platform→digest
  mapping (limits in R1).
- **Two independent verification anchors** per object: its filename, and the registry serving the same
  digest. A published `o/` object has exactly one today.
- **Decode is unconditional**, so a copied subtree resolves without knowing where it came from.
- **Absent dispatch recovers offline, in both provenance kinds.** An installed package's image index is
  staged into `$OCX_HOME/blobs` at install time, and a tag's `content` is a digest the registry serves, so
  `recover_absent_dispatch` (`chained_index.rs:291-347`) finds the blob and self-heals it back into `o/`
  for a published source exactly as for a derived one. The leaf-trap — a digest no registry blob endpoint
  can serve, because the bot minted it — has no surface left.
- **One invariant with no exceptions** beats a rule plus an absence convention. The "is `content` in `o/`?"
  branch and the "what does absence mean for this source kind?" question both disappear.
- **Attestation-descriptor divergence disappears.** ocx's projection silently skips platform-less
  descriptors (`pipeline.rs:220-222`) while the bot's `_platforms_from_index` raises `KeyError` on them
  (`observe.py:105-113`, deliberately uncaught). Verbatim bytes have no such fork.

**Negative / accepted**

- **Derived indices lose bare-manifest tags.** A foreign single-platform repository's tags cannot be locked
  into a derived index (D5). They resolve live under Default; they are unresolvable under `--offline`. The
  cost of no exceptions. ocx-published repositories are unaffected.
- **Objects grow ~1.5–1.7×** (R2). Absolute sizes stay sub-KB to low-KB.
- **CAS dedup narrows** from "same platform set" to "same image index". Aliased tags (a cascade's
  `1.0.0`/`1.0`/`1`/`latest` pointing at one pushed index) still share one object — their `content` digests
  are equal by construction, which is the index's "emergent aliasing" rule
  (`bot/src/indexbot/model.py:99-110`). What is lost is dedup between two *distinct* image indices that
  project to an identical platform set — the lossy-projection dedup this ADR removes on purpose. The
  `ObservationObject` docstring's "hence maximal dedup" claim (`model.py:138-146`) no longer applies.
- **The index carries publisher-controlled bytes it did not author** (R4).

---

## Risks

### R1 — The security claim, precisely (and its limits)

**Closed:** fabrication of a platform→digest *mapping* by a party with index write access but no registry
write access. Today such a party edits one small bot-authored JSON file, recomputes its sha256, updates
`tags[tag].content`, and every client check passes — the client re-derives the digest
(`ocx_index.rs:640-648`) and each leaf digest independently, and none of those checks says anything about
whether the mapping was ever real. After this change the attacker must produce bytes a registry actually
serves under that digest.

**Not closed:**

- **Substitution of a whole, genuine index.** Repointing `tags["3.28"].content` at a different real image
  index (an older release, another package) still verifies. Only fabrication is closed, not misdirection.
  Closing that needs signing — still a Non-Goal (`adr_index_indirection.md` §Non-Goals).
- **Publisher authentication.** Nothing attests *who* pushed the index.
- **The `repository` pointer.** Still index-authored, still transport-only, still TOFU on change. SSRF
  remains the only floor (`oci/ssrf.rs`, `ocx_index.rs:692-714`).
- **Client-side detection.** The bot's `verify_claims` (`verify_claims.py:90-110`) re-derives from registry
  truth at PR/reconcile time — governance on the public index, not something a client runs. The delta here
  is on the **client**: the mapping arrives as registry-served bytes, not as a claim.

### R2 — Object size growth (quantified; the registry-served figure is computed, not fetched)

**Measured.** The vendored golden
`crates/ocx_lib/tests/fixtures/index_wire/observation/sha256/750b2589….json` — two `linux/amd64` leaves
differing only by `os.features` — is **340 bytes**.

**Computed** (byte count over the OCI image-index grammar, minified, no annotations): the same two
descriptors as a raw index add `"schemaVersion":2,` (18 B) + the index `mediaType` (54 B) at the top level,
and per descriptor the manifest `mediaType` (57 B) + `"size":<n>,` (≈12 B) — about **+72 B fixed and +69 B
per platform**, i.e. ~548 bytes at two platforms (**≈1.6×**), ~1.5× at five. Publisher `annotations` or
`artifactType` add on top.

**Not verified:** I could not fetch the live `ghcr.io/michael-herwig/ocx-e2e-hello` index or the object in
ocx-sh/index#57 — this agent has no network or shell tool, and the working tree holds no `o/` object for
that package (§Migration). The figures are a measured local vector plus an analytic delta, not a live
measurement. Confirm before quoting externally.

**Disposition: accept.** Sub-KB objects, one per tag, against the ~6× shrink `adr_index_indirection.md` A3
already banked by dropping the manifest chain.

### R3 — The canonical-tag exclusion is load-bearing, not cosmetic

D4(b) is the one place the hard rule could break something ocx itself created. `push_canonical_tag` is
**default-on** (Decision E) and writes a bare platform manifest under `sha256.<hex>`
(`client.rs:387-398`). The bot's `observe()` walks `list_tags()` unfiltered except for `__ocx.desc`
(`observe.py:191-209`). Ship the rule without the exclusion and the nightly reconcile sweep refuses every
ocx-published repository. This is a required, simultaneous change, not a follow-up.

### R4 — Optional OCI fields: parsing and trust surface

- `annotations`, `artifactType`: publisher-controlled strings now stored. The index already ingests one
  publisher-controlled string with a scheme allowlist — `org.opencontainers.image.source`
  (`observe.py:35-47, 130-144`) — precisely because it lands as an `href` on a public page. **Storing** them
  is inert; **rendering** them is not. The site renders neither today (`PlatformMatrix.vue:36-42` reads
  platform + digest only); keep it that way absent a deliberate decision.
- `subject`: the vendored fork's `OciImageIndex` (`external/rust-oci-client/src/manifest.rs:342-374`) has no
  `subject` field and no `deny_unknown_fields`, so a `subject` parses fine and is ignored. Harmless
  *because* the bytes are stored verbatim and never re-serialized — a parse-then-write path would silently
  drop it. This is why the verbatim-bytes rule (A4) is load-bearing.
- `OciManifest` is `#[serde(untagged)]` with `Image` before `ImageIndex`
  (`external/rust-oci-client/src/manifest.rs:44-53`). An image index has neither `config` nor `layers`, so
  it cannot match the `Image` arm — the same discrimination D4(a)'s check relies on, and the bot's
  `"manifests" in raw` test (`observe.py:177-181`) is its Python equivalent.
- **Descriptor `platform` is optional, and the absent case is the dangerous one.** Every enumeration over
  an index now sees the attestation and referrer descriptors the projection filtered out — marked either
  by the placeholder `unknown/unknown` or by omitting `platform` entirely. The placeholder is inert: it is
  a platform OCX cannot represent, and propagating that `TryFrom` error would abort a whole enumeration
  over one descriptor nobody asked to select. Omission is not: `TryFrom<Option<..>>` answers
  `Platform::Any`, and an `Any` **offer** satisfies **every** requirement
  (`adr_platform_model_unification.md` D1), so one platform-less descriptor is a universal match and two
  make every selection ambiguous. Both cases are therefore simply not candidates, decided by one shared
  predicate — `Platform::candidate_from_descriptor` (`oci/platform.rs:142-145`), consumed by
  `Index::fetch_candidates` for selection and by `Platform::from_image_index` for display
  (`ocx index list --platforms`); a non-candidate is skipped at `debug`, never an error. The index-side
  enumerations (`_catalog_platforms`, `PlatformMatrix.vue`) carry the same rule. Listed in §What Breaks.

### R5 — Does digest-verifying a registry-served index need anything new?

No. `resolve_observation` already recomputes `sha256` over the fetched bytes and hard-errors on mismatch
(`ocx_index.rs:640-648`); `write_dispatch_object` recompute-and-verifies on write (A4);
`common::verify_requested_digest` (`common.rs:441-453`) exists for this class. The only change is that the
digest now *also* has meaning to a registry — a verification option added, not a requirement.

### R6 — Does any consumer read a field beyond `platforms`?

No. Grepped both repos: the type has one field (`wire.rs:105-109`; `model.py:138-146`;
`schema/observation-object.schema.json:7-15`), and every reader — `observation_to_index`
(`ocx_index.rs:777-803`), `render.py:139-158`, `useObservation.ts:21-23`, `PlatformMatrix.vue:36-42`,
`verify_claims.py:98-110` (digest only) — touches `platforms[]` and nothing else. All enumerated in
§What Breaks.

---

## Open Questions

- **OQ1 — should announce verify that referenced platform manifests carry canonical tags?**
  **CLOSED — no. Owner ruling 2026-07-25.**

  Recorded because the question is natural and should not be reopened without new reasons.

  Two grounds. First, **the premise was wrong**: manifest liveness does not rest on canonical tags. A
  manifest stays online while *anything* references it — any tag, and equally any image index that still
  lists it. A manifest referenced by an older, still-referenced index survives with no tag of its own. So
  a missing canonical tag does not imply a vanishing manifest, and the check would refuse announces over
  a condition that is not the failure it claims to detect.

  Second, and decisive regardless: **OCX adopts OCI, it does not enforce a particular OCI usage.** Keeping
  manifests reachable is the registry operator's and the publisher's concern. Canonical tags and the
  documented build-timestamp guidance are *tools OCX offers* to cover an error-prone case — a tag moved or
  a patch released such that nothing points at an older manifest any more. Turning an offered safeguard
  into an admission requirement would make the index arbiter of how publishers run their registries, which
  is precisely the coupling this ADR removes elsewhere.

- **OQ2 — should the check also require `artifactType == application/vnd.sh.ocx.package.v1`, or is "is an
  image index" the right granularity?** **CLOSED — no. Document kind is the right granularity.**
  Owner ruling 2026-07-25.

  Recorded because the question is natural and should not be reopened without new reasons. The gate never
  inspects `artifactType`: not as a refusal, and **not as a warning** either.

  **Four grounds, each independently sufficient.**

  First, **the invariant could not stay single.** D3's value is one rule with no exceptions and no
  source-kind conditionality — D6 deletes exactly that conditionality from A3. An `artifactType`
  requirement cannot be stated as one invariant over `o/`: a derived index stores a plain registry's image
  index verbatim (§Context; `persist_dispatch` → `stage_dispatch_bytes`, `local_index.rs:442-462`), and a
  foreign multi-arch image carries no OCX
  artifact type. The gate would therefore either delete derived indexing over foreign registries — a
  capability D5 priced and deliberately kept — or apply to published sources only, reintroducing the
  per-source-kind rule this ADR removes.

  Second, **it refuses indices ocx itself maintains.** `merge_platform_into_index` fills `artifact_type`
  only when it is absent and leaves a declared foreign value alone (`client.rs:335-341`, D5) — overwriting
  it would relabel someone else's artifact. An index another tool created under its own artifact type
  therefore keeps it across every subsequent ocx push into it. The refusal would name the publisher for a
  property of the artifact's history whose only remedy is to hand-author and force-push an index — the
  registry-practice dictation this ADR removes elsewhere, reached by a different road than OQ1's.

  Third, **it stops no adversary.** `artifactType` is a publisher-controlled string inside
  publisher-controlled bytes, in a namespace announce already requires the publisher to have claimed
  (`AnnounceError::UnclaimedNamespace`, `announce/error.rs:60`). A hostile publisher writes the OCX string; an
  honest one whose index predates the field cannot. A gate the attacker clears by choosing a value and the
  honest party cannot clear at all inverts the sign of an admission control.

  Fourth, **the failure that matters is closed one layer down, at the right layer.** `pull.rs:398` enforces
  `MEDIA_TYPE_PACKAGE_V1` on the resolved **leaf** manifest — the document ocx authors end to end — so
  installing a non-OCX artifact fails regardless of what any index catalogued. That is the pattern the
  codebase follows: ocx enforces `artifactType` on manifests it authored whole (`pull_description`,
  `client.rs:1290-1299`; `fetch_single_layer_artifact`, `client.rs:1411-1418`) and on no document it merges
  into. Nothing in either repo reads an image index's `artifactType` — a gate on a field no consumer
  consults buys nothing.

  **The middle option — warn rather than refuse — is rejected, not deferred.** A warning must name an
  action, and the only action available is hand-authoring an index (ground two). An absent `artifactType`
  on an index ocx maintains is a common benign state, which project doctrine gives `debug` and
  self-healing, not `WARN`; announce is machine-facing, so a diagnostic CI must learn to filter is worse
  than none. "Refuse only on a *known-foreign* type" additionally obliges OCX to curate other people's
  artifact types, permanently, and still yields nothing against ground three.

- **OQ3 — the byte-exactness CI gate for CAS objects.** `cli/validate.py`'s byte-exact discipline re-derives
  the canonical form and compares (`bot/CONTRACTS.md:965-973`). Registry-served bytes cannot be re-derived
  by the bot, so the `o/` gate becomes D4(c) (hashes to filename, parses as an image index), with registry
  equality covered separately by `verify_claims`. Whether that lands as a §14 amendment or a §12 change to
  `cli/validate.py` is an index-repo editorial call. **OPEN.**

- **OQ4 — `<hex>.json` extension.** Raw image-index bytes are JSON, so the existing CAS naming
  (`cas_relpath`, `validate_entry.py:333-341`) fits and the desc-blob extension convention is undisturbed.
  No change proposed; recorded because a reviewer will ask. **Closed unless someone objects.**

---

## Validation (contract, not implementation)

- [ ] Every file under `p/<ns>/<pkg>/o/<algo>/<hex>.json` parses as an OCI image index, in both provenance
      kinds; no code path decodes any other shape.
- [ ] `sha256(bytes) == <hex>` **and** `GET /v2/<physical-repo>/manifests/<algo>:<hex>` against the root's
      `repository` returns byte-identical content.
- [ ] Every `tags[].content` in every committed root has an object in `o/` — no tag entry is ever absent
      from `o/` (D2/D3).
- [ ] A curated tag resolving to a bare image manifest is **refused** with `TagIsNotAnImageIndex`, naming
      the tag and repository, in both the ocx announce path and the bot.
- [ ] A repository carrying `sha256.<hex>` canonical tags and `__ocx.desc` sweeps clean: both excluded from
      the observe universe, zero refusals, zero recorded entries (R3).
- [ ] A hosted `p/<ns>/<pkg>` subtree copied verbatim into `$OCX_HOME/index/<any-source>/` resolves a tag →
      platform-manifest digest offline, with no source-kind configuration on that path.
- [ ] `ocx package announce` writes as the CAS object the exact bytes `fetch_manifest_raw_bytes` returned,
      under the digest it returned — no re-serialization anywhere in the announce path.
- [ ] `grep -r Observation crates/` and `grep -r ObservationObject bot/` return nothing outside changelogs
      and this ADR.
- [ ] A published-source tag whose `o/` object is deleted recovers from the physical registry by digest and
      self-heals into `o/` (the path that 404s today).
- [ ] A digest-addressed pinned leaf (`pkg@sha256:…` from `ocx.lock`) still resolves and still writes
      nothing to `o/` — the D2 scope boundary holds.
- [ ] An image index carrying an attestation descriptor resolves correctly and appears in no platform
      listing.
- [ ] `ocx index update` output for a published package is byte-identical to `wget --mirror` of the same
      subtree.
- [ ] A tag whose index was superseded by a later platform push (old index GC-eligible, gone from the
      registry) still resolves from the snapshot in `o/` — the case the snapshot exists for (Context,
      point 3).
- [ ] `ocx package announce --tags __ocx.desc,__ocx,__ocxfoo,__OCX.desc,sha256.<hex>,1.2.3` announces
      `1.2.3` only, and emits a notice naming each of the five dropped tags (D7).
- [ ] `ocx index list` omits all five reserved forms; `Tag::from` classifies each as `Internal` or
      `Canonical`, including the `sha256.<hex>` **dot** form and the case variants.
- [ ] A hand-authored PR whose root carries a reserved tag is **rejected** by the index repo's own
      validation, with no ocx involvement anywhere in the path (D7 third layer).
- [ ] `ocx package push` reports the canonical tags it wrote in `PushReport`; `--announce-file` contains
      no canonical tag.
- [ ] The shared tag-verdict fixture is consumed by both test suites, and changing one implementation's
      verdict without the other fails a test in that repo.

## Links

- [`adr_index_indirection.md`](./adr_index_indirection.md) — the ADR this amends; A1/A4/B/C2/C3/E/G/H stand
- [`adr_platform_model_unification.md`](./adr_platform_model_unification.md) — `select_best`, the lock unit
- [`adr_announce_publisher_surface.md`](./adr_announce_publisher_surface.md) — the announce CLI surface whose
  CAS payload this changes
- ocx-sh/index: `bot/CONTRACTS.md` §1/§7/§14, `schema/{root,observation-object}.schema.json`,
  `.claude/artifacts/adr_locked_observation_index_format.md` (ADR-1 D3/D4 — the dedup and platform-set
  rationale this supersedes)
- [OCI image-index spec v1.1.1](https://github.com/opencontainers/image-spec/blob/v1.1.1/image-index.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-25 | architect (opus) | Initial ADR. `o/` holds verbatim OCI image indices; the invented per-index object is deleted. Projection claim verified (`announce/pipeline.rs:204-238`, `bot/src/indexbot/core/observe.py:105-113`); raw bytes + registry digest already available and discarded at both producers (`pipeline.rs:181`, `ghcr.py:217-228`). Supersedes `adr_index_indirection.md` A2/A3/C1/D-corollary/F1/F4 clause by clause (D6); R4 explicitly **not** resurrected. Migration measured: one root, zero CAS objects, Track E unannounced. Size delta computed ~1.6× at two platforms, **not** verified live (no network tool). |
| 2026-07-25 | architect (opus) | Owner amendment 3 — reserved-tag rule folded in as **D7**: `__ocx*` (no dot, case-insensitive) and canonical `sha256.<hex>` are never versions; verdict moves onto `Tag` itself (`tag.rs:103-117`) replacing the weaker `is_internal_str` string check at `oci/index.rs:247-256`; announce filters **once** on the resolved selection (`package_announce.rs:100-109`), with `--refresh`/`--tags-file` noted as carriers not sources (`announce.rs:292-295`); three layers, three verbs (push **reports**, announce **ignores** with a notice, the index **rejects**); the `PushOutcome` gap (`publisher.rs:42-51`) routed to `PushReport` (`api/data/push.rs:21-42`), explicitly not `--announce-file`; shared cross-repo tag-verdict fixture pinned, citing the `fake_forge.py` precedent; `#57`'s cleanliness recorded as omission, not policy. **New finding:** `Tag::Canonical` does not match the tag form ocx writes — `Digest::try_from` requires the colon form (`oci/digest.rs:223`) while `push_canonical_tag` writes the dot form (`client.rs:392-394`), so `Tag::from("sha256.<hex>")` returns `Other` today; without fixing that, the new verdict compiles and silently never fires. Consistency: Context point 4 and the D5 over-determination paragraph no longer claim canonical tags keep manifests alive; the OQ1 forward reference now reads as closed. |
| 2026-07-25 | architect (opus) | Owner amendment 2 — the actual rationale. Context now leads with the **GC asymmetry**: a manifest is stable by identity and kept online by its canonical tag (verified: `options/canonical_tag.rs:29` default-on, `publisher.rs:85-90, 114-118`, `cascade.rs:228-230`, tests `publisher.rs:376`/`:397`), whereas an index is a collection over platforms — adding one produces a new digest, the tag moves, and the old index becomes GC-eligible during ordinary correct publishing (`client.rs:315-336`). Hence: snapshot exactly what can disappear. Division of responsibility stated as the reproducibility contract. D1 gains the root argument — **a snapshot you cannot verify against the thing it replaces is not a snapshot, it is an assertion** — weighted above the lossy-projection point, and shown to be `adr_index_indirection.md` Decision D's own exemption clause ("no later re-resolvable fetch exists for the doctrine to protect against", `subsystem-oci.md`) carried to its conclusion rather than reversed. D5 records that the indices-only rule is over-determined: a bare manifest needs no snapshot at all, so refusing it declines to record something carrying no information. New **OQ1** (canonical-tag verification at announce) with the trade-off and the `publisher.rs:88-90` finding that per-push canonical tagging cannot cover pre-existing index entries, so only a whole-index check can observe the violation; former OQ1/2/3 renumbered to OQ2/3/4. |
| 2026-07-25 | architect (opus) | Owner amendment. (1) Reframed around OCI adherence as the principle — the index is a catalog of OCI artifacts and defines no shapes of its own; tags float, the index locks what one resolved to; copy-paste demoted to a consequence; "observation object" retired as a noun. (2) **Indices only, enforced, no exceptions** — the A3 absence rule is deleted for tag entries rather than preserved; D4 places the check at three layers (per-tag refusal, sweep-universe exclusion, CI shape gate) and names the failure `TagIsNotAnImageIndex`; `_platforms_from_bare` is deleted, not adapted. Scope boundary added: digest-addressed leaves are content addressing, untouched. (3) Verified `ocx package push` always publishes an index (`client.rs:302-311` fresh, `:276-298` wraps a bare manifest, `:315-323` entry, `:332-336` push; artifactType at `:294`/`:307`) — the rule refuses nothing OCX produces. **New finding (R3):** `push_canonical_tag` (`client.rs:387-398`, default-on per Decision E) writes bare manifests under `sha256.<hex>` tags that `observe()` currently walks unfiltered, so the D4(b) exclusion must ship in the same change or reconcile refuses every ocx-published repo. Derived-index cost of the no-exceptions rule stated rather than carved around (D5). OQ1 replaced with the artifactType-granularity question, weighted by `client.rs:299` (a pre-existing index is carried through and never re-stamped). **Superseded 2026-07-26 (row below):** that last reading of `client.rs:299` was wrong — `merge_platform_into_index` *does* stamp `artifact_type`, but only into an absent field (`client.rs:335-341`); a declared foreign value is what survives untouched. D5 and OQ2 ground two carry the current account. |
| 2026-07-26 | builder (opus) | Realignment review-fix pass. D4(b) no longer claims a flat exit `0` for an explicitly curated `sha256.<hex>`: the mixed selection exits `0` with a stderr drop notice (`package_announce.rs:164-171`), while a *wholly* reserved selection collapses into `AnnounceError::NoCuratedTags` (`announce/pipeline.rs:141-143`) and exits **64** with the names in the error text (`announce/error.rs:191`, pinned at `:259`). OQ2's stale anchors corrected — the verbatim-storage site is `persist_dispatch`/`stage_dispatch_bytes` (`local_index.rs:442-462`, not `validate_catalog_key`), and the two `artifactType` enforcement sites are `client.rs:1290-1299` / `client.rs:1411-1418` (the former anchors landed on a doc comment and a signature). The §What Breaks "synthetic-index adapter" anchor was dropped: the adapter is deleted and `ocx_index.rs:768-776` is now `sync_catalog`. Changelog row above corrected against D5. **F8 from the prior review is withdrawn as a false alarm** — `persist_dispatch` writes no root entry; `commit_root_tag` does, and both derived write paths gate on `records_root_tag` (`local_index.rs:880-890`), which refuses a non-`ImageIndex` manifest before any tag pointer is written (`chained_index.rs:559`, `local_index.rs:238`). D2, D3 and the Consequences bullet stand as written; no owner decision is pending. |
