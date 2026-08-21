# ADR: Registry-to-registry package copy (`ocx package copy`)

<!--
Architecture Decision Record — MADR format.
Owner: Architect (/architect). Handoff: Builder (/builder), Security Auditor (/security-auditor), QA (/qa-engineer).
-->

## Metadata

**Status:** Proposed
**Date:** 2026-08-19
**Deciders:** @michael-herwig, architect
**GitHub Issue:** TBD (this ADR precedes the tracking issue)
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Follows the Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024, Tokio,
      no new dependency; composes the shipped OCI client, publisher and cascade modules
**Domain Tags:** integration, security, api, infrastructure
**Supersedes:** —
**Superseded By:** —

---

## Context

A corporate deployment runs separate **dev / staging / prod** registries. OCX can publish a
package (`ocx package push`) from local build artifacts and can *read* through a corporate
mirror (`adr_oci_registry_mirror.md`), but it has no way to move an artifact that already
exists in one registry into another.

The workaround — rebuild and re-push against the next registry — is not equivalent, because
the leaf platform-manifest digest changes, and that digest is load-bearing twice:

1. **Signatures.** `ocx package sign` resolves the per-platform target and signs the **leaf**
   manifest digest; the Sigstore bundle's `subject` descriptor names it
   (`crates/ocx_lib/src/oci/sign/pipeline.rs:109-124`). A rebuilt leaf has a different digest,
   so every existing signature refers to an artifact that no longer exists at that coordinate.
   The staging attestation cannot be carried into prod.
2. **Lockfiles.** A V2/V3 `ocx.lock` pins the leaf digest directly
   (`adr_lock_records_physical_address.md` D3); install resolves that digest content-addressed
   and never consults an index. A rebuild invalidates every downstream pin.

So the property a promotion pipeline exists to provide — *the bytes that passed the staging
gate are the bytes running in prod* — is currently unexpressible in OCX.

### What a published package actually is

This is the crux, and getting it wrong produces a plausible implementation that destroys data.
A package in a repository is **three distinct kinds of object**:

```text
cmake/
  tag 3.28.1          → image index { linux/amd64→M1, darwin/arm64→M2, ... }
  tag 3.28            → image index  (rolling, per-platform merged)
  tag 3               → image index
  tag latest          → image index
  tag sha256.<hexM1>  → M1 directly        (canonical safety-net tag, one per LEAF)
  tag __ocx.desc      → description artifact (README + logo), repo-level
  referrers(M1)       → sigstore bundle    (subject = M1, never the index)
  blobs               → config + layers
```

| Object | Nature |
|---|---|
| Leaf platform manifest + blobs | immutable, content-addressed; what the lock pins and the signature subjects |
| Tag → image index | a **mutable set keyed by platform**, not content |
| Rolling tags | derived pointers, computed from a **platform-aware** blocker check |

`adr_cascade_platform_aware_push.md` already records what happens when the second row is
treated like the first: its Bug 2 is a single-platform index pushed verbatim over a
multi-platform rolling tag, silently deleting every other platform's entry. A naive
"copy the tag" implementation is that same bug, replayed across registries.

### Platform is not recoverable from a leaf

`crates/ocx_lib/src/package/metadata/authoring.rs:203` hard-rejects a `platform` field in
package metadata:

> the `platform` field is no longer part of package metadata; `ocx package create --platform
> <PLATFORM>` records it in a build receipt beside the bundle instead

Platform therefore lives in exactly two places — the **build receipt** (local, build-time) and
the **image-index entry** (registry). A leaf manifest carries no platform information anywhere.
A copy addressed by tag can read the platform off each index entry; a copy addressed by a leaf
digest cannot derive it at all. This is a design fact, and it is what forces `--platform` to be
*required* rather than merely convenient for a digest-addressed source.

---

## Decision Drivers

- **Digest preservation is the whole feature.** Anything that changes a leaf digest defeats the
  purpose; the mechanism must make that a contract, not a coincidence of a deterministic builder.
- **No data loss on the target.** A promotion must never remove a platform, tag or referrer the
  target already holds and the source never had.
- **Reuse the shipped machinery.** `merge_platform_into_index`, `push_with_cascade`,
  `push_canonical_tag`, the referrers capability probe and the `Publisher` facade already exist
  and already encode the hard-won per-platform semantics.
- **Fail early and locally.** An invocation that cannot succeed must be rejected before the
  first network write, and be provably so.
- **Bounded memory.** Toolchain layers are 100–200 MB; a copy must not scale RAM with layer size.
- **Supply-chain integrity.** A copy is a write into a higher-trust environment; the read side
  is therefore a trust boundary, not merely a fetch.

---

## Industry Context & Research

**Research artifact:** N/A — the surveyed prior art is directly comparable and is summarised here.

**Trending approaches.** The ecosystem verb is **copy**, not *promote*: `oras cp --recursive`,
`crane copy`, `skopeo copy`, `regctl image copy --referrers --digest-tags`. "Promotion" in
Artifactory and Harbor is a *policy layer* over a copy (a build-promotion API, a replication
rule), never a distinct transport primitive.

**Key insights:**

1. **Referrers must be copied explicitly and recursively.** `oras cp` needs `--recursive` and
   `regctl` needs `--referrers`; neither copies the referrer graph by default, and users
   discover the omission when a signature fails to verify at the destination. OCX should
   default the flag **on** — a promotion that silently drops provenance is the failure mode.
2. **Digest-named tags are a separate copy axis.** `regctl`'s `--digest-tags` exists precisely
   because a digest-named tag is not reachable by walking a manifest. OCX has exactly such a tag
   (`sha256.<hex>`, `adr_index_indirection.md` Decision E), so a copy must write it too or the
   target's tag graph fails `ocx package cascade check`.
3. **No surveyed tool understands a rolling-tag cascade**, because none of them has one. Every
   general-purpose copier can move the content and none can maintain `3.28` / `3` / `latest`
   correctly, which is the specific gap that makes this an OCX command rather than a
   documentation page pointing at `oras cp`.

---

## Considered Options

### Option 1 — Document `oras cp`, build nothing

**Description:** Ship a docs page telling operators to use `oras cp --recursive` followed by
`ocx package announce`.

| Pros | Cons |
|------|------|
| Zero code, zero maintenance | Cannot maintain the rolling-tag cascade — the target's `3.28`/`latest` are left wrong or untouched |
| Uses a maintained, well-tested tool | Does not write the `sha256.<hex>` canonical tag, so the target fails `cascade check` |
| No new CLI surface to keep stable | A second binary and a second credential configuration in every corporate pipeline |

### Option 2 — Extend push: `ocx package push --from <SRC>`

**Description:** One command; the layer source is either local files or a remote repository.

| Pros | Cons |
|------|------|
| Smallest CLI surface; reuses `Publisher::push_cascade` unchanged | `--metadata`, `--build-timestamp` and `-f/--file` all become conditionally invalid, and the help text must explain two modes |
| "Copy is push with a different source" is an accurate mental model | Push *builds* the leaf manifest; a copy must not, so the two modes diverge exactly where it matters most |
| No new report type | `--build-timestamp` in copy mode would silently rename a promoted artifact — a footgun that has to be special-cased anyway |

### Option 3 — New `ocx package copy`, byte-copying the tag's image index

**Description:** A new command that fetches the source tag's index and pushes those exact bytes
to the target tag, then copies the leaves it references.

| Pros | Cons |
|------|------|
| Simplest possible implementation; index digest preserved too | **Destroys target platforms absent from the source** — `adr_cascade_platform_aware_push.md` Bug 2 across registries |
| One code path regardless of what the target holds | A filtered (`--platform`) copy would silently truncate the target tag to one platform |
| — | Pins an index digest, which is a snapshot of a mutable set — not a meaningful content identity |

### Option 4 — New `ocx package copy`, per-platform upsert (**chosen**)

**Description:** A new command. Leaf manifests and blobs are copied byte-for-byte; each leaf is
**merged** into the target tag's index per platform via the existing
`Client::merge_platform_into_index`; rolling tags are recomputed against the **target** registry
via the existing `push_with_cascade`.

| Pros | Cons |
|------|------|
| Digest-preserving, so signatures and lock pins survive | New CLI surface to keep stable |
| Cannot destroy a target platform — `merge_platform_into_index` upserts and preserves siblings | Two readings of `--platform` (filter vs declaration) depending on source form |
| Reuses the platform-aware cascade math, so rolling tags stay correct at the target | Needs a new transport method for bounded-memory blob transfer |
| Own flag set, so `--metadata`/`--build-timestamp` simply do not exist | — |

---

## Decision Outcome

**Chosen Option:** Option 4.

**Rationale.** Options 1 and 3 are both wrong for the same underlying reason: they treat a tag's
image index as content, when it is a mutable per-platform set that only OCX knows how to
maintain (rolling tags, canonical tags, platform-aware blockers). Option 2 is the right *mental*
model — a copy really is a push with a remote source — but the one place the two diverge is the
one place that matters: push **builds** the leaf manifest and a copy must **never** rebuild it.
Encoding that divergence as a mode flag on `push` would leave three flags conditionally invalid
and put the most dangerous one (`--build-timestamp`, which renames the artifact) one typo away
from a silent mis-promotion.

### The four decisions this ADR fixes

**D1 — Leaf manifests are byte-copied, never rebuilt.** `manifest_builder` happens to be
deterministic today (no timestamps), so a rebuild *might* reproduce the same bytes. Relying on
that means one future annotation or media-type change silently invalidates every signature in
the fleet. The copy path reads raw bytes with `pull_manifest_raw` and writes them with
`push_manifest_raw`, and a structural guard test asserts the module never reaches a manifest
builder or re-serializes a leaf.

*Amendment 2026-08-19, during implementation.* This decision originally added "and the digest the
target registry reports on push is compared against the source's", by analogy with the sign
pipeline at `oci/sign/pipeline.rs:175`. That comparison is not available on this path:
`push_manifest_raw` returns the `Location` URL the registry answers with, not a digest, so there
is nothing to compare. What actually holds the property is stronger and needs no round trip — the
manifest is PUT to a digest-addressed URL, so a registry that stored different bytes under that
name is already violating the distribution spec, and the blob path re-hashes every spooled blob
locally (`verify_spooled_blob`) before it is uploaded. The end-to-end proof is the acceptance
test that fetches the raw manifest from both registries and compares bytes and
`Docker-Content-Digest`.

**D2 — Indexes are merged per platform, never byte-copied.** Via
`Client::merge_platform_into_index` (`crates/ocx_lib/src/oci/client.rs:428`), whose
retain-then-insert is already covered by `existing_index_adds_platform` and
`existing_index_replaces_same_platform`. Three consequences follow and are accepted:

- a platform present in the target but absent from the source is **kept**, and reported as
  `kept (not in source)` so a filtered copy cannot silently leave a mixed index;
- a platform present in both with a **different** digest is **replaced** — the displaced leaf is
  not orphaned, because its `sha256.<hex>` canonical tag still names it and its referrers stay
  attached to that digest;
- a pre-existing index carrying two entries for one platform **self-heals** to one, because the
  retain removes every match.

Copy must *not* deduplicate by digest: the same digest legitimately appears under two platforms
for platform-agnostic content, and the canonical-tag dedup in `PushOutcome` already assumes it.

**D3 — Rolling tags are recomputed against the target, never copied.** `existing_versions` for
`push_with_cascade` is obtained from `list_tags` + `parse_versions` on the **target** identifier.
A source that holds `3.28.2` which never passed the gate must not cause the target's `3.28` to
skip `3.28.1`; conversely a target that already holds a newer patch must block the rolling move.
The platform-aware blocker check in `resolve_cascade_tags` provides this unchanged.

**D4 — Source reads use canonical addressing, never the mirror path.** A copy's read result
*becomes* the bytes written into a higher-trust registry, which is precisely the case
`subsystem-oci.md` invariant #5 covers: a read that decides a write must not be mirrored.
Reading a tag through a poisoned corporate mirror would otherwise let it choose what gets
promoted into prod. Source resolution otherwise follows the sign pipeline verbatim —
`Index::select` → `physical_reference` → `guard_physical_dial` (the fail-closed SSRF re-check at
the dial site) → transport reference.

### Consequences

**Positive**
- A promoted artifact is bit-identical to the tested one; its staging signature verifies
  unchanged at the target, and every `ocx.lock` pin against it stays valid.
- Rolling tags and the canonical tag remain correct at the target, so `cascade check` passes
  after a promotion without a repair step.
- The target's existing platforms, tags and referrers are never destroyed.

**Negative**
- `--platform` carries two readings by source form (filter for a tag, declaration for a digest).
  Documented in the help text and the reference page rather than split into two flags, because
  the alternative is two flags that are each invalid half the time.
- One new transport method (`push_blob_from_path`) and a stub-transport extension are prerequisites.

**Risks**
- *Partial failure mid-copy leaves a half-promoted tag.* Mitigated by a two-phase write order:
  phase 1 writes blobs, leaf manifests and referrers (all pure adds, invisible until a tag
  names them) for every platform; phase 2 does the index merges, the canonical tags and the
  cascade — see the amendment under "Write order" for why the canonical tag cannot be phase 1. A
  crash in phase 1 leaves the target untouched; a crash in phase 2 is partial but re-runnable,
  and the digest-compare below makes the re-run cheap.
- *Unbounded memory on large layers.* `OciTransport::push_blob` takes `Vec<u8>` and
  `NativeTransport::do_push_blob` deliberately holds the whole blob in RAM so a transient fault
  can restart from a refcounted `Bytes`. At 100–200 MB layers and concurrent platforms that is
  the allocation `package-manager-domain.md` PKG-04 exists to prevent. Mitigated by a new
  `push_blob_from_path`: spool with the existing `pull_blob_to_file`, then open a fresh `File`
  per attempt into the fork's already-streaming `push_blob_stream`. Replayable by construction,
  bounded memory, retry semantics unchanged.
- *Target registry without a Referrers API.* Fails closed with `ExitCode::ReferrersUnsupported`
  (84) when referrers are requested, matching the sign path. No fallback tag scheme is
  introduced — OCX is referrers-only by design (#106).

---

## Technical Details

### Source-form contract

Resolved and rejected **before any network write**:

| Source form | Platform | Target | Violation |
|---|---|---|---|
| `repo:tag` → image index | read from each index entry | `--to` (host rewrite) or `-i/--identifier` | — |
| `repo@sha256:<leaf>` / `repo:sha256.<hex>` → image manifest | `--platform` **required** | `-i/--identifier` **required** | exit 64 naming the missing flag |
| `repo@sha256:<index>` → image index | — | — | exit 64: an index digest is a snapshot of a mutable set; name the tag |
| `--to` with `-i/--identifier` | — | — | clap conflict, exit 64 |

Index-vs-manifest discrimination reuses `Index::fetch_candidates` (`oci/index.rs:460`): the
`Manifest::Image(_)` arm yields one candidate with no platform discrimination, the
`Manifest::ImageIndex(_)` arm yields one per entry.

### Per-platform disposition

Before transferring a platform, the target tag's current index entry is compared with the source
leaf digest:

| Target entry | Action | Reported |
|---|---|---|
| absent | full copy | `added` |
| present, same digest | skip blobs, leaf and merge entirely | `unchanged` |
| present, different digest | full copy, then upsert | `replaced` |
| present, platform not in source | untouched | `kept (not in source)` |

This is what makes a re-copy after one new platform was published cost exactly one platform, and
any repeated copy a near-no-op.

*Amendment 2026-08-21, review-fix loop.* The `unchanged` row as written promises more than the
implementation does. `copy_leaf` (`crates/ocx_lib/src/oci/copy.rs:119-186`) runs unconditionally
for every selected platform except under `--dry-run` — the target's index entry proves the leaf
manifest is *present*, not that every blob it names still is
(`crates/ocx_lib/src/publisher/copy.rs:197-201`). An `unchanged` platform therefore still re-PUTs
its leaf manifest and, with `--referrers` (the default), re-copies its full referrer chain —
`copy_referrers` (`oci/copy.rs:326-398`) has no existence check against the target before calling
`push_referrer_manifest`, and its `seen` set is scoped to one `copy_leaf` call, not to a run
across re-invocations. Only blob *bodies* are skipped, via the target HEAD at
`oci/copy.rs:256-259`. The corrected row: "present, same digest → skip blob bodies (HEAD-checked);
re-verify by re-PUTting the leaf and re-copying referrers → `unchanged`". This is idempotent in
effect (no new content, no tag movement) but not free on the wire, which is what makes a repeated
promotion of a signed package cost a manifest PUT and a referrer re-copy on every run, not zero
requests.

### Write order

```text
phase 1 — pure adds, invisible until a tag names them
  for each selected platform:
      blobs (mount_blob when same registry, else spool + stream)
      leaf manifest (raw bytes, PUT to the digest-addressed URL)
      referrers (recursive: a signature over an SBOM is a referrer of a referrer)
phase 2 — the only mutations a reader can observe
  for each selected platform:
      merge_platform_into_index(target tag)
      sha256.<hex> canonical tag
  cascade: 3.28 / 3 / latest, computed from the TARGET's tag list
```

*Amendment 2026-08-21, review-fix loop.* The canonical tag is written in phase 2, not phase 1 as
the block above originally had it. `Client::push_canonical_tag` derives the platform's leaf digest
from the *merged* image index its own doc comment names as `merged_manifest` — that index does not
exist until `merge_platform_into_index` has run, so the canonical-tag write cannot precede it. The
call site in `publisher::copy::copy` sits inside the phase-2 tag loop, after the merge, and takes
the merged index as its subject. (Cited by symbol rather than by line: the line numbers this
amendment originally carried had already drifted by the end of the same review round.) Consequence: a crash between the
index merge and the canonical-tag write leaves a moved tag with no `sha256.<hex>` safety net for
that run — recoverable by re-running the copy, since the merge is itself re-verified on retry, but
not the phase separation this section originally described.

### CLI contract

```text
ocx package copy <SOURCE>
    [--to <REGISTRY> | -i, --identifier <REF>]
    [-p, --platform <PLATFORM>]...
    [-c, --cascade]
    [--canonical-tag | --no-canonical-tag]
    [--referrers | --no-referrers]
    [--description]
    [--annotation <K=V>]...
    [--dry-run]
```

Deliberately absent: `--metadata` (arrives inside the leaf), `-f/--file` (no local layers), and
`--build-timestamp` (would rename a promoted artifact). Output format stays the root-level
`--format plain|json`.

`ocx package describe` gains `--from <SOURCE>` so a repo-level description can be promoted on
its own; `ocx package copy --description` is the opt-in that folds it into a version copy. The
default is off, because a version-level copy silently rewriting a repo-level README is a surprise.

### Exit codes

No new code. 64 usage, 65 malformed manifest, 79 source not found, 80 auth, 81 `--offline`
refusal, 84 referrers unsupported at the target.

*Amendment 2026-08-21, review-fix loop.* Dropped `--frozen` from the 81 row: `copy` reads
`context.remote_client()` directly (`crates/ocx_cli/src/command/package_copy.rs:121`), which
is gated on `options.offline` alone (`crates/ocx_cli/src/app/context.rs:228`) — `--frozen`
only refuses unpinned-tag *resolution* at the project/package tier
(`app/context.rs:270-274`), which `copy` never performs.

---

## Implementation Plan

1. [ ] `StubTransport` capture for `push_referrer_manifest` / `list_referrers` (today
       `unimplemented!()`), and for the new blob method — blocking prerequisite for every test.
2. [ ] `OciTransport::push_blob_from_path` (native + stub).
3. [ ] `oci/copy.rs` — transfer engine, two-phase, bounded concurrency, canonical source reads.
4. [ ] `Publisher::copy` / `copy_cascade` / `copy_description`.
5. [ ] `command/package_copy.rs`, `command/package_describe.rs --from`, `CopyReport`.
6. [ ] `error_envelope.rs` `collect_context` / `collect_detail` arms for the new error kind
       (both are hardcoded to `SignError`/`VerifyError` today, so the JSON `detail` slug would
       otherwise be silently empty). *In progress in the 2026-08-21 review-fix loop (WP-B/WP-D)
       — `cli/classify.rs` already carries `try_downcast!(CopyError)`; `error_envelope.rs` itself
       was untouched as of `review_r1_spec_package_copy.md` finding A5.*
7. [ ] Unit and acceptance suites, including the second zot registry fixture.
8. [ ] Docs, casts, command reference, rule + handshake amendments.

## Validation

- [ ] A signed package copied to a second registry verifies at the target with no re-sign.
- [ ] Leaf manifest bytes and `Docker-Content-Digest` identical on both sides.
- [ ] A filtered copy into a target holding another platform leaves both platforms present.
- [ ] Every source-form violation exits 64 with the target registry provably never contacted.
- [ ] A second identical copy performs no upload and reports every platform `unchanged`.
- [ ] Security review of the canonical-addressing decision (D4) and the referrer copy path.

## Links

- [`adr_cascade_platform_aware_push.md`](./adr_cascade_platform_aware_push.md) — the per-platform index semantics this depends on
- [`adr_index_indirection.md`](./adr_index_indirection.md) — logical/physical resolution and the canonical `sha256.<hex>` tag (Decision E)
- [`adr_lock_records_physical_address.md`](./adr_lock_records_physical_address.md) — why the leaf digest is what a lock pins
- [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md) — referrers-only signing, capability probe, exit 84
- [`adr_oci_registry_mirror.md`](./adr_oci_registry_mirror.md) — the read-path half of the corporate story
- [ocx-sh/ocx#198](https://github.com/ocx-sh/ocx/issues/198) — attestations, the natural home for a "approved for prod" re-sign at the target

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-08-19 | architect | Initial draft |
