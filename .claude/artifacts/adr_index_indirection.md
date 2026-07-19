# ADR: The Index — One Format, Many Copies (Wire-Grammar Local Index, Dispatch-Only Objects, Partial Snapshots, Logical Identity)

## Metadata

**Status:** Accepted — Decisions A–H. Decision F (index.ocx.sh client consumption) is built against the
frozen ● wire shapes in §Data Model (the only contract surface); live `ocx-sh/index` M-1 verification is
a post-merge smoke, not a build gate.
**Date:** 2026-07-18
**Deciders:** mherwig (decisions D-A…D-E locked and user-approved in
[`handover_index_indirection.md`](./handover_index_indirection.md); this ADR specifies them)
**Beads Issue:** N/A
**Related issues:** [#215](https://github.com/ocx-sh/ocx/issues/215) (the local index copy is not
self-contained — a version choice cannot resolve offline), [#212](https://github.com/ocx-sh/ocx/issues/212)
(list of platforms)
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024 / Tokio; no new
  external dependency — reworks existing stores + adds one typed seam)
**Domain Tags:** oci, index, storage, resolution, gc, project
**Supersedes:** `design_spec_registry_indirection.md` (2026-07-12, unimplemented — physical-keyed
storage overturned by Decision C)
**Superseded By:** N/A
**Depends on:** [`adr_platform_model_unification.md`](./adr_platform_model_unification.md) (Accepted —
`is_compatible` / `select_best` / canonical grammar / lock V3 feed every decision here)

---

## Context

### The Index — one format, many copies (the conceptual spine)

There is **one** concept in this ADR: **an index**. An index is a single artifact format — the wire
grammar frozen in §Data Model: a `config.json`, a catalog `c/index.json`, per-package roots
`p/<ns>/<pkg>.json`, and per-package dispatch objects `p/<ns>/<pkg>/o/<algo>/<hex>.json`. Everything else
here is downstream of that one fact. Indices differ along exactly three axes, never in format:

- **Location.** An index can be *hosted* (index.ocx.sh), *mirrored* (a corp static file server),
  *shipped* (a devcontainer feature's directory, a CI artifact, a directory committed to a repo), or
  *local to a machine* (`$OCX_HOME/index/<source>/`). Copying an index anywhere it can be read — `rsync`,
  `wget --mirror`, `git add`, a container layer — leaves it an index. That property *is* why "copy a
  mirror" works: there is nothing to convert.
- **Provenance.** An index is either **published** — a snapshot of a remote ocx-index (index.ocx.sh or a
  mirror of it): bytes copied/synced from it and verifiable against it — or **derived** — created *by* OCX
  from an OCI registry's tags API, because an OCI registry publishes no index of its own. Same format both
  ways; a derived index is OCX-authored in that format, a published one is copied in it. (Where the
  code-level distinction matters this ADR calls a published index an *index source* and a derived one an
  *OCI source*.)
- **Completeness.** A local copy is normally **partial and per-package**: `ocx index update <pkg>` grows
  it one package at a time (that package's root plus its dispatch object). A full site mirror
  (`wget --mirror`) is just the degenerate *complete* case. Partial and complete copies are
  format-identical — a partial copy is a subset of entries in the same grammar, nothing special.

The **local index home is a collection**, not a single index: `$OCX_HOME/index/ocx.sh/`,
`$OCX_HOME/index/ghcr.io/`, … — one index per source, side by side. A *hosted* index is always exactly
one. Redirection (`--index` / `OCX_INDEX`) points OCX at a different collection (a shipped copy) instead
of the machine-local one.

Every decision below hangs off this spine and should read as downstream of it: dispatch-only `o/` is
*what the format stores* (Decision A); recompute-and-verify is a *format property* (Decision A);
`ocx index update` / `ocx index catalog` *keep a copy current* at two granularities (Decision F);
`[registries]` config picks *which remote index a namespace uses* (Decision F); `--index` / `OCX_INDEX`
pick *which local collection you read* (Decision A).

### What an index is for — and what it is NOT for

An index answers exactly one question: **which versions of a package exist, and for a chosen version,
which platform-manifest digest and physical location does it resolve to.** That is *tag / version
resolution*. Nothing more.

An index is **not** part of `ocx.lock` resolution. A V2/V3 lock already records the pinned
platform-manifest leaf digest directly (`adr_platform_model_unification.md` D3); resolving a lock reads
that leaf digest and fetches it content-addressed — it never consults an index to do so. Any earlier
framing of the local index as "the store that makes lock-pinned resolution offline" was wrong: locks are
already index-free. This correction is the spine of this revision.

Where the local index earns its keep is **free version *choice***: a devcontainer parameter, a Gradle
property, or a CLI tag that selects *among available versions* must resolve deterministically and without
the network. #215's "self-containment" complaint reframes to exactly this — a *shipped* index copy (a
devcontainer feature, a CI artifact, a committed repo directory) must let a version choice resolve to a
concrete platform-manifest digest with zero network dependence and zero dependence on machine-global
state. That, and only that, is what redirection (`--index` / `OCX_INDEX`) exists for.

### Today's local copy is not yet a self-contained index

Verified against `feat/platform-model-unification`, three defects keep the current local copy from being a
usable index:

1. **Redirection is half-wired.** `OCX_INDEX` / `--index` redirects only the tag store; the manifest bytes
   it points at live in the machine-global `$OCX_HOME/blobs` (`context.rs:184-192`). A shipped copy
   therefore resolves a tag → digest but then reaches into a global store a clean machine does not have —
   not self-contained (the #215 defect, reframed: a shipped copy cannot resolve version choice offline).
2. **`ocx index update` writes tag pointers only** — "strictly a tag refresh" (`command/index_update.rs`),
   no dispatch objects. The copy holds no per-platform `[(Platform, Digest)]` to run `select_best` over
   offline, so platform selection for a chosen version still needs the network.
3. **Stored bytes don't hash to their recorded digest.** The write path re-serializes via
   `serde_json::to_vec_pretty` (`local_index.rs`), so an object's bytes disagree with the digest that
   names it — the copy is not verifiable against itself and re-push / air-gap replay is foreclosed
   (Differentiator #9).

Decision A fixes all three: redirect the whole collection (A1), write dispatch objects verbatim (A3),
recompute-and-verify on write (A4).

### The public index is arriving in a new shape

The upstream `index.ocx.sh` is being finalized as a **pointer index, not a registry** (no `/v2`, no
blobs): a `p/<ns>/<pkg>.json` root (volatile `repository` pointer + `tags → content`) plus immutable
`o/sha256/<hex>.json` observation objects (`platforms[].digest`). The stale
`design_spec_registry_indirection.md` predates this shape and keys storage on the *physical* registry
location — which breaks on the human-PR storage migrations the index is designed for. This ADR specifies
how OCX consumes that shape (Decision F) and keys everything on logical identity (Decision C). It does
**not** re-specify `ocx.lock` V3 (that is `adr_platform_model_unification.md` D3 — carried forward
verbatim), and it does **not** put the index in the lock's resolution path (see above).

---

## Decision Drivers

- **DR1. Deterministic, offline version resolution.** A shipped index copy must resolve a *version
  choice* — a tag selecting among available versions — to a concrete platform-manifest digest **and**
  physical location with zero network and zero dependence on machine-global state. This is *tag / version
  resolution*, not lock resolution: locks are already index-free (`adr_platform_model_unification.md` D3).
  The *dispatch* (tag → digest → platform selection) is what must be self-contained (#215); content bytes
  (the leaf manifest and its layers) are fetched on demand and content-address-verified — the copy bundles
  the resolution pin, not the content.
- **DR2. Verifiability over invention.** Every stored object must hash to a digest OCX can recompute
  against registry truth. No invented digest namespaces; no computed-then-stored derivations.
- **DR3. Logical identity is stable; physical location is volatile.** `repository` pointers migrate by
  human PR. Storage paths and GC roots must key on logical identity so a migration never orphans a store
  or drifts a GC root (D-C). Any committed reference that names a package (a pin, an index entry) likewise
  survives migration because it names logical identity plus a content digest, never a physical host.
- **DR4. One relation, one lock unit.** Content-less references (locks, pins, public-index entries)
  pin the **platform-manifest digest**, never the image-index digest — uniform with
  `adr_platform_model_unification.md` (D-A).
- **DR5. Pre-1.0 clean break.** No migration code, no compat shims, no vestiges. The old `TagLock`
  path is deleted; `ocx index update` regenerates (`project_breaking_compat_next_version`,
  `feedback_refactor_as_if_never_existed`).
- **DR6. Boring, GC-safe storage.** Reuse the existing CAS write/verify primitives (atomic
  tempfile+rename, digest recompute-and-verify). The object store is the hosted wire layout verbatim —
  per-repo `p/<repo>/o/<algo>/<hex>.json`, full-hex-named (A2), a deliberate divergence from
  `BlobStore`'s two-level `cas_shard_path` shard (`cas_path.rs`). Keep the local index outside the GC
  sweep so a machine-local `ocx clean` can never collect a shipped copy's data.

---

## Scope

**In scope (this ADR takes these decisions):**

| # | Decision | Open point resolved |
|---|---|---|
| A | Local index copy = hosted wire grammar verbatim; object store holds **dispatch objects only** (manifests never copied); collection home; write path | §7.1, §7.2 |
| B | Blob-store & GC integration (leaf manifests live in the blob store, fetched on demand) | §7.3, §7.4 |
| C | Resolution pipeline: logical → index resolve → physical → mirror_map → fetch | D-C |
| D | Lock doctrine + snapshot exemption | D-A |
| E | Canonical tags (`--[no-]canonical-tag`) | D-D |
| F | index.ocx.sh client consumption + routing config (`[registries]` identity, `[mirrors]` policy) | §7.5, §7.7 |
| G | Old `TagLock` fate | §7.6 |
| H | Index component model (`LocalIndex` / `OcxIndex` / `OciIndex` / `IndexSync` / `ChainedIndex`; one remote per namespace) | new |

**Out of scope (non-goals — do not fold in):**

- **Signing.** Deferred to a future ADR. The mix-and-match threat is recorded verbatim in Non-Goals
  below (D-E).
- **Index-repo internals** — announce / reconcile / render / seed-import workflows, the index-bot
  sort-key bug fix, ADR-1 D8 opt-in→default-on alignment. Their backlog, not ours.
- **M-1 gating** — the 42-package republish proving format v1 stable. Decision F builds against it;
  Decisions A–E, G, H are independent and may land first (handover work item #3).
- **Public-index seeding / namespace Amendment A1** (`ocx.sh/ocx/cli` reserved segment) — watched, not
  built.
- **`ocx.lock` V3 re-spec** — owned by `adr_platform_model_unification.md` D3. Carried forward
  verbatim; this ADR only states the lock-unit doctrine and the snapshot exemption.

---

## Decision A — Local index: verbatim hosted grammar, dispatch-only object store

### A1. Home — the local collection, one root (§7.1)

**Decision: `LocalIndex` owns the local index *collection* — one index per source under a single home —
so a shipped copy resolves a version choice offline without reaching into machine-global state.**

The status-quo split — a redirected tag store while the manifest bytes stay machine-global
(`context.rs:184-192`) — is deleted. `LocalIndex` roots every per-source subtree at one home, chosen by
trivial precedence:

| # | Source | Note |
|---|---|---|
| 1 | `--index PATH` | Highest precedence — point at a shipped copy explicitly (`context_options.rs:110-114`) |
| 2 | `OCX_INDEX` env | A shipped copy's deployment (a devcontainer feature, a CI step) sets this |
| 3 | `$OCX_HOME/index` | Default — the machine-local collection; **not** `$OCX_HOME/tags` |

The home is a **collection**: `<home>/ocx.sh/`, `<home>/ghcr.io/`, … one index per source. Pointing
`--index` / `OCX_INDEX` elsewhere swaps the *whole* collection for a shipped one — never a partial
overlay, never a second format. What looks like "two locations" is two copies of the same format with
different lifecycles: a shipped copy (travels with a devcontainer / CI artifact / repo) versus the default
machine-local collection.

**No repo path is architected here.** A shipped copy may live anywhere a deployment chooses — a
devcontainer feature's directory, a CI artifact, a directory committed to a repo (conventionally `.ocx/`,
but nothing requires it). OCX is pointed at it by `--index` / `OCX_INDEX`; how a deployment produces and
ships the copy is that deployment's business, not this ADR's.

The default sits at `$OCX_HOME/index` — a first-class store sibling to `blobs/`, `layers/`, `packages/`,
`symlinks/`, not runtime state buried under `state/`: it is a *directory of index copies*, exactly the
owner's framing ("the OCX index is just a directory where a local copy of an index is stored"). The
`FileStructure` / `LocalIndex` home field is named `index` accordingly. The machine-global
blob/layer/package CAS
(`$OCX_HOME/blobs|layers|packages`, `file_structure.rs:67-69`) is a *separate* concern (Decision B): it
holds installed **content**; the index collection holds resolution **dispatch**. `$OCX_HOME/tags` (the
old tag-store root, `file_structure.rs:70`) is removed from `FileStructure`.

**No ambient `OCX_INDEX` (owner decision 2026-07-18).** The direnv export never sets `OCX_INDEX`. An
ambient whole-collection pin would redirect *every* index operation in that shell — including host-scope
ones (a machine-global `ocx index update`, a host-level version query) — into whatever copy the project
ships, which is the wrong scope; a constant export would also stomp a user-set value. And there is no
reproducibility reason to make it ambient: locks never needed the index anyway (they read the pinned leaf
digest directly), so the only thing an ambient pin buys is surprise. The deliberate channels are an
explicit `--index` and a deployment-set `OCX_INDEX` — nothing ambient. Home precedence (`--index` ▸
`OCX_INDEX` ▸ `$OCX_HOME/index`, `context.rs:187-196`) is otherwise unchanged.

### A2. On-disk layout — a local copy IS the hosted wire grammar (owner decision 2026-07-18)

**Decision: each source's subtree under the home is the hosted served tree, byte-for-byte grammar. No
local re-encoding.** A source's subtree is:

```
<home>/<source>/
├── config.json                 (published indices only — {"format_version": 1})
├── c/index.json                (published indices only — catalog: {"<ns>/<pkg>": "sha256:<root-digest>"})
├── c/index.json.etag           (published indices only — conditional-GET validator sidecar)
└── p/<ns>/
    ├── <pkg>.json              root doc
    └── <pkg>/o/<algo>/<hex>.json   object CAS (dispatch objects only; A3)
```

The two provenance kinds (Context spine) share the grammar and diverge only in who authored the bytes:

- **Published index (`index.ocx.sh` and mirrors of it) — copied bytes.** The local subtree is a literal
  copy of the hosted site: an `rsync` / `wget --mirror` of the site into `<home>/<source>/` is a valid
  *complete* copy; an `ocx index update`-grown subtree is a valid *partial* one (same grammar, fewer
  entries). It is **self-verifying** with no OCX-minted metadata — every `o/…/<hex>` filename is the
  sha256 of its bytes, and every `p/<ns>/<pkg>.json` root is verified on read against the digest its
  `c/index.json` catalog entry carries (F1) — so an auditor checks the copy against nothing but itself:
  objects by filename, roots by catalog digest.
- **Derived index (a plain OCI registry, no ocx-index protocol) — OCX-authored bytes.** There is no site
  to copy (an OCI registry publishes no index), so OCX writes the root doc itself in the same grammar:
  `{ "repository": "oci://<physical>", "tags": { "<tag>": { "content": "sha256:<dispatch-digest>",
  "observed": "<iso8601>" } } }`. No `config.json`, no `c/index.json` — a derived index's catalog is the
  directory enumeration of `p/`.

`observed` is the ISO-8601 timestamp the pointer was last confirmed against the source; it drives the
volatile-root refresh of Decision F and answers "when was this taken". It is **not** a freshness gate for
local resolution (Invariant 1 — the local copy is never auto-invalidated, `subsystem-oci.md`).

**Consequences of adopting the grammar verbatim:**
- The human-governed lane — `status`, `yanked`, `superseded_by`, `deprecated_message` — lives in the
  root doc and therefore survives locally *for free*: `ocx package info` surfaces a yank or deprecation
  **offline**, straight from the copied root, with no separate store (Decision F3).
- The catalog is **per-source** (each published index carries its own `c/index.json`; a derived index
  uses directory enumeration) — there is no global merged catalog to keep coherent.
- The root doc's `repository` field is the **physical** pointer; **logical identity comes from the path**
  (`<source>/p/<ns>/<pkg>`). Storage never keys on `repository` — this is Decision C made structural by
  the layout itself.

### A3. The object store holds dispatch objects only — manifests are never copied (owner decision 2026-07-18, headline)

**Decision: `p/<repo>/o/` holds *dispatch objects* only — observation objects (published indices) and
image indexes (derived indices). A leaf platform manifest is never written into the local index.** The
copy pins **volatility** — the tag → digest binding and the platform → digest dispatch that a re-push can
change. Everything below a digest is immutable, content-addressed, and fetchable on demand from any
registry; it is **content, not a pin**, and belongs in the machine-global blob store (Decision B), not the
local index.

**Absence disambiguates itself.** A tag's `content` digest in the root doc is either a dispatch object or
a bare manifest:

1. `content` **present** in `o/` → decode it (obs object for a published index, image index for a derived
   index) → `select_best(host, [(Platform, Digest)])` → leaf platform-manifest digest → fetch that leaf
   from the physical registry, verify OCI CAS.
2. `content` **absent** from `o/` → by construction it names a **manifest** (a single-platform tag points
   its `content` straight at the platform manifest) → resolve it via the blob store, then a registry
   fetch-by-digest.

The absent case cannot be confused with a damaged snapshot, because the fallback fetch decodes the bytes
(`decode_snapshot_manifest`, the existing codec dispatch, `local_index.rs:404-428`):
- bytes decode as an **image index** ⇒ this was a multi-platform tag whose image index was missing from
  `o/` (an incomplete snapshot) ⇒ **self-heal**: write the image index into `o/` (`write_object`
  verify-and-self-heal, `snapshot_store.rs:208`) and continue at step 1;
- bytes decode as a **manifest** ⇒ the leaf path (step 2) is correct.

For a published index the obs object always travels with the copy (A2), so absence there is a damaged
copy that re-fetches the obs object from the index site.

The recursive child-walk that once snapshotted the whole image-index → platform-manifest chain
(`persist_manifest_chain`, `local_index.rs:150-169`, and both its production callers —
`LocalIndex::refresh_tags`, `chained_index.rs:259`) is **deleted**: writing the single dispatch object
is the whole write.

### A4. Write path — keep raw bytes, recompute + verify digest, self-heal (§7.2)

**Decision: the client keeps the raw response bytes; the tree writes them verbatim; the digest is
recomputed from those bytes and verified against the source-claimed digest before the write commits.**

Fixes the re-serialization defect (`local_index.rs:207` `serde_json::to_vec_pretty`) directly:

| Step | Today | After |
|---|---|---|
| Client fetch | the tag-resolve path parses then **discards** raw bytes (`client.rs:1471-1482`, facts §2) | raw bytes retained alongside the parsed value — generalize the `fetch_manifest_raw_bytes` precedent (`client.rs:1391-1412`, already used by the managed-config CAS) to the index path |
| Object write | `to_vec_pretty(manifest)` → bytes ≠ digest (`local_index.rs:202-214`) | verbatim bytes written to `p/<repo>/o/<algo>/<hex>.json` |
| Digest handling | source digest trusted, never recomputed | `sha256(bytes)` recomputed; **must equal** the source-claimed digest or the write aborts (`DataError`, CWE-345 class) |

Every stored object hashes to its filename, so the tree is offline-verifiable and re-push replays
byte-identical content. Verification failure is a hard error — an object whose bytes disagree with its
claimed digest is untrusted input at a trust boundary (`quality-core.md`). The genuinely new work over
the `fetch_manifest_raw_bytes` precedent is the recompute-and-verify step; the same primitive
(`write_object`) backs the A3 self-heal write.

---

## Decision B — Blob-store & GC integration

### B1. The local index is outside the GC reachability graph (§7.4)

**Decision: the local index is user/deployment-managed data, never walked by GC.** `LocalIndex` is not one
of the three GC-walked stores. GC walks only `packages/layers/blobs.list_all()` (facts §3;
`reachability_graph.rs:71-77`); the local index is invisible to it, exactly as the old tag store was. A
machine-local `ocx clean` collecting a shipped copy's data would be a data-loss bug (DR6).

Consequence: no new `CasTier` variant, no `list_all()` join, no root/edge wiring. The local index lives
*outside* the graph — the cheaper and safer half of the "in vs out" choice (R6), matching its
user/deployment-managed lifecycle.

### B2. Leaf manifests and layers live in the blob store; the local index holds only dispatch (§7.3)

**Decision: the local index (dispatch) and the machine-global blob store (content) are distinct stores
with distinct lifecycles; both hold verbatim bytes; neither replaces the other.**

- **Leaf platform manifests and their layers** are content: they live only in the GC-tracked
  `$OCX_HOME/blobs`, fetched by digest on demand (A3) and linked via `refs/blobs/` when a package is
  installed (`ResolvedChain` → `link_blobs`, `subsystem-package-manager.md`). That path feeds GC
  reachability and re-push/air-gap replay of *installed* content, unchanged in shape — its bytes are now
  verbatim (A4), a strict improvement (re-push byte-exact). Installed-tool **offline exec** is therefore
  unaffected by A3: the leaf manifest was cached here at install time.
- `add_index_retention_edges` (`reachability_graph.rs:302-346`) is **retained unchanged**. It parses
  each `≤4 MiB` blob as an image index and adds a child-manifest → index-blob retention edge so a live
  platform manifest keeps its parent index alive. For **index.ocx.sh-resolved** packages there is no
  image index in the fetched chain (Decision C bypasses it), so the parsed-as-`ImageIndex` guard
  (`reachability_graph.rs:366`) does not match the flat platform-leaf blob — correct by construction, no
  code change.

A **dispatch object** (an image index) may exist both in a local `o/` and in `$OCX_HOME/blobs` (where an
installed package's `add_index_retention_edges` parses it): distinct lifecycles — the local index travels
with its copy, the blob store with the machine. Leaf manifests are never in the local index, so there is
no manifest duplication. Collapsing the stores would recouple the index copy to the GC sweep (R2).

---

## Decision C — Identity is logical; location is routing (D-C, overturns the stale spec)

**Decision (copied from D-C, locked): storage paths, `ocx.lock`, and GC roots key on LOGICAL identity
(`ocx.sh/<ns>/<pkg>`), never physical. `index.ocx.sh` is not a registry — pointers only, no blobs, no
`/v2`. The OCI image index is fully bypassed for index-resolved packages.**

### C1. Pipeline order

```
logical id (ocx.sh/<ns>/<pkg>[:tag])
  → index resolve   : GET /p/<ns>/<pkg>.json (root) → tags[tag].content (obs digest)
                      → GET /p/<ns>/<pkg>/o/sha256/<obs-digest>.json (verify sha256 of bytes)
                      → platforms[] : select_best(host, [(Platform, Digest)]) → leaf digest
  → physical        : root.repository ("oci://ghcr.io/ocx-contrib/cmake") — a distinct type
  → mirror_map      : Client::transport_reference rewrite ([mirrors] seam)
  → fetch           : GET physical-registry /v2/.../manifests/<leaf-digest> (verify OCI CAS)
```

- `index.ocx.sh` yields **only pointers**; the platform manifest and its layers come from the physical
  registry named by `repository`. The image-index hop that a normal OCI resolve performs
  (`Index::select` over a stored image index) is skipped entirely — the obs object already carries the
  per-platform `[(Platform, Digest)]` that `select_best` consumes (`adr_platform_model_unification.md`
  D1).

### C2. Structural enforcement — the `[mirrors]` precedent

**Decision: physical location is a distinct type that cannot compile into a storage path**, copying the
mirror seam precedent (`adr_oci_registry_mirror.md`): the physical rewrite is confined to one seam
(`Client::transport_reference`, `client.rs:116-135`), and the transport-only reference "never
round-trips into storage" (`client.rs:124-126`). The blanket `Identifier ↔ transport` conversion is
removed so no storage-path construction can name a physical location by accident. `index.ocx.sh` adds a
second such seam (logical → physical via the root `repository` pointer) obeying the same rule: the
`repository` value is transport input, never a storage key.

- **Why logical, not physical:** the `repository` pointer is volatile *by design* (human-PR storage
  migration). Keying storage (or any committed reference — a lock, a pin) on `.physical()` (the stale
  spec's D-approach) breaks it the moment a maintainer migrates the backing registry. Logical identity +
  content digests survive: the digest verifies the identity from *any* physical location. (Keying is the
  only place the lock and the index meet — a shared discipline, not a resolution dependency; a lock never
  consults the index, D.)
- **GC hazard (verified, D-C):** reachability recomputes roots via the same
  `slugify(identifier.registry())` that writes use (`file_structure.rs:104-105`). Any logical/physical
  drift → silent orphan → collected next `clean`. Logical keying = zero drift by construction.

### C3. `oci://` scheme marker is a one-way door

**SETTLED (2026-07-19): keep `oci://`.** The `oci://` scheme on a root doc's `repository` is a **type
marker and wire contract**: strict parse — a missing scheme is `ResolveError::MalformedEntry`, a CI
host-allowlist hook applies, and it is a forward door for future repository kinds. Both source kinds use
it: verbatim on an index-source copy, OCX-authored on an OCI source (A2). There is **no local-only
scheme** — source kind is named by the path `<source>` segment and the config registry kind (Decision F),
never by a scheme marker: logical identity comes from the path, `repository` is transport-only.

**Rationale: `oci://` marks the reference kind, never transport.** `repository` travels inside shared
identity data — the root document a package's identity resolves through — so its scheme cannot double as
a transport selector without handing transport control to whoever authors that data. Transport is chosen
host-side instead: the target scheme comes from a `[mirrors]` entry for that host (`adr_oci_registry_mirror.md`)
or the plain-HTTP allowance in `OCX_INSECURE_REGISTRIES`. An `http://`/`https://` scheme on `repository`
would let a publisher force every consumer's fetch to plaintext — the same class of downgrade the
`[mirrors]` insecure-host gate exists to prevent elsewhere. Index-side, this is settled; **do not
relitigate**.

**Roadmap note.** A future `[registries."<host>"] insecure = true` knob (host-scoped, operator-set) is the
sanctioned path if a plain-HTTP *registry* ever needs first-class config support beyond today's
`OCX_INSECURE_REGISTRIES` allowlist — not a scheme change on `repository`.

---

## Decision D — Lock unit doctrine + snapshot exemption (D-A)

**Decision (locked): the lock unit is the platform-manifest digest, never the image-index digest —
uniform across `ocx.lock`, dependency pins, and the public index.** Already Accepted:
`adr_platform_model_unification.md` D3 (V3 stores the platform-manifest digest, "the outer image-index
digest is intentionally not stored") and index-side ADR-1 D1.

This doctrine is a *shared keying / verification discipline*, not a resolution dependency. Resolving an
`ocx.lock` reads its pinned leaf digest and fetches it content-addressed — it never consults an index (see
Context "What an index is for"). The lock and the index meet only here, on what unit a content-less
reference names, and (Decision C) on keying identity logically. The index's role is version *choice*, not
lock resolution.

### The snapshot exemption (written explicitly, as the handover demands)

The manifest-digest doctrine governs **content-less references that cross time or trust** — a lock, a
pin, a public-index entry. Such a reference must name the exact bytes it authorizes because the bytes
travel *separately* (fetched later, from a registry, by a different machine or a later CI run): an
image-index digest would let the platform set be re-resolved at fetch time against a mutated index —
the tampering surface the doctrine closes.

The local index's `content` pointer is **exempt** because it names a **dispatch object held in `o/`** —
an image index (derived index) or an observation object (published index) — whose bytes travel *with* the
pointer and are recompute-verified on read (A3/A4). The dispatch object fixes the platform → leaf-digest
mapping locally, so there is no later, re-resolvable fetch to tamper with; `select_best` runs over
digest-verified, locally-present bytes. The **leaves** the dispatch object names are the doctrine-correct
**platform-manifest** digests — the lock unit itself — each independently OCI-CAS-verified when fetched
from the physical registry on demand (Decision C, A3). Storing an image-index digest as `content` is
therefore not a lock; it is a verified CAS root over a fixed dispatch.

**Corollary (consistency, not contradiction):** `ocx.lock` — content-less — always records the
platform-manifest leaf digest per `adr_platform_model_unification.md` D3. The local index — dispatch-
carrying — records as its `content` the **image-index digest** (derived index) or the **observation-object
digest** (published index), because those bytes are present in `o/`. All are correct: the exemption is
about *whether the named bytes travel with the pointer*, not about the store.

This ADR does **not** re-specify `ocx.lock` V3 — that stands as `adr_platform_model_unification.md` D3.

---

## Decision E — Canonical tags (D-D)

**Decision (locked): `--[no-]canonical-tag` on `ocx package push`, default ON (flatten-flag style).**

| Aspect | Decision |
|---|---|
| Flag | `--[no-]canonical-tag`, default **ON** (opt-out), on `ocx package push` |
| Effect | Push a digest-named tag `sha256.<hex>` per pushed **platform manifest** |
| Purpose | Pure registry-side deletion safety net (a stray tag delete cannot orphan a digest still pinned by a lock) |
| Index interaction | **`index.ocx.sh` ignores canonical tags entirely** — no wire semantics, not required, never announced |

Because the local index no longer holds leaf manifests (A3), an index-resolved leaf is fetched from the
physical registry on demand, so the leaf must stay alive registry-side. Canonical tags are exactly that
safety net: a `sha256.<hex>` tag per platform manifest keeps every leaf tag-reachable, and an image-index
child stays reachable via the image-index tag. This is the registry-GC counterpart to the local index no
longer carrying content (A3 consequence).

This is a registry-side concern only; the local index and the resolution pipeline never *read* canonical
tags. (Index-repo ADR-1 D8 says "opt-in"; D-D is the later decision — default-on wins; the flip is an
index-side alignment task, out of scope here.)

---

## Decision F — index.ocx.sh client consumption

**Decision: consume the ● frozen wire shapes below; treat everything else as evolving. Only the ●
shapes are contract.** Built **now** against the frozen ● shapes (owner decision 2026-07-17,
superseding the earlier M-1 build gate): acceptance-tested via a local static-file HTTP fixture that
encodes the exact wire bytes; live verification against the real index is a post-merge smoke once M-1
lands. The client design spec is re-derived against these shapes (handover waiting-on #3).

### F1. Two-hop fetch + cache lifetimes

| Object | Volatility | OCX behaviour |
|---|---|---|
| `config.json` (● `{"format_version": 1}`) | version pin | Read once; reject unknown `format_version` (fail-closed, `DataError`) |
| `p/<ns>/<pkg>.json` **root** (●) | **volatile** (human-PR `repository` migration, tag curation) | Snapshot-first under Default — **never auto-refreshed** (Invariant 1; local-first-except-`--remote`, `subsystem-oci.md`). The snapshot copy is dated by `observed`. A live re-fetch happens **only** on an explicit `ocx index update` or a `--remote` resolve, which rewrites the snapshot and bumps `observed`. Offline-first (DR1, Principle #2) means a Default-mode resolve never reaches upstream for the root. Every read **verifies `sha256(bytes)` against the `c/index.json` entry** for a source with a synced catalog (below) |
| `o/sha256/<hex>` **observation object** (●) | **immutable** (CAS) | Fetch once, **verify `sha256(bytes)` == `<hex>`**, cache forever in `o/sha256/` |
| `c/index.json` (● `{"<ns>/<pkg>": "sha256:<root-digest>"}`) | volatile catalog | Whole-catalog sync via **conditional GET + digest diff** (F2) |
| `desc` blobs (`.md`/`.svg`/`.png`) | **frozen-status UNRESOLVED** | **OPEN interop point — record, do not build on it** (F4) |

The obs-object verify step is load-bearing: it is the primary place OCX re-derives a digest OCX did not
mint, so it anchors trust for everything below a dispatch object. A mismatch is a hard `DataError`.

**Root verification under partial copies — crash-safe upsert, inconsistency vs staleness (read path).**
A root moved out of the shared `o/` CAS to its own `p/<ns>/<pkg>.json` path (A2), so it carries no
self-naming filename digest the way an obs object does — the catalog entry pins it, and that entry is
nothing but `sha256` of the root's own bytes. `ocx index update <pkg>` writes in a **fixed order, each
step idempotent** — not one atomic operation:

1. the dispatch object into `o/` (CAS write by content hash; an orphan left by an aborted write is
   harmless — nothing points at it yet);
2. the root `p/<ns>/<pkg>.json` (tempfile + atomic rename, DR6's write primitive);
3. the package's `c/index.json` catalog entry upsert (tempfile + atomic rename of the catalog file).

A crash between any two steps leaves a **recoverable** state, never a rejected one — there is no window
in which an interrupted update makes the copy look corrupt.

- **The catalog entry is derivable, not independently trusted.** It is exactly `sha256(root bytes)` and
  nothing more. A read-path mismatch between a stored root and its catalog entry is therefore an
  **inconsistency**, not evidence of anything adversarial, and it recovers **deterministically**:
  re-derive the entry from the root bytes actually on disk and rewrite the catalog to match. This logs at
  info/debug — a root written but not yet caught up by its catalog entry (steps 2 and 3 straddling a
  crash) is the ordinary, benign cause, not a warn-worthy event. No journal, no generation pointer, no
  write-ahead log: recovery is recomputation, because the entry was never anything but a cached derivation
  of the root.
- **Hard `DataError` is reserved for actual corruption** — cases recomputation cannot fix because the
  source bytes themselves are bad: an unparseable root document, dispatch-object bytes whose `sha256`
  disagrees with their own `o/` filename (A4), or a `repository`-field cross-check failure. A root/catalog
  digest disagreement alone is never one of these; it is always resolved by rewriting the catalog from the
  root, never by refusing the read.
- **Remote-catalog drift = staleness ("update available"), never an error.** A later catalog sync (F2)
  that fetches the *remote* catalog and finds a **different** digest for an already-snapshotted package is
  reporting that the remote moved on — the local pin is *stale*, not *wrong*. The local catalog is the
  snapshot's own record of what was pinned, not a live query; staleness is resolved by re-snapshotting
  that package (steps 1-3 again) — never by erroring, never by silently overwriting the local root.

**Trust framing, stated honestly.** The root/catalog check on read catches corruption, partial copies,
and interrupted updates — real failure modes with a deterministic fix. It does **not** detect adversarial
substitution: the catalog entry is *computed from* the root, so a party able to rewrite a root can
recompute and rewrite its catalog entry identically — root and catalog share one trust domain, not two
independent ones. Outer integrity still comes from TLS (a live fetch) or git / filesystem trust (a shipped
or rsync'd copy), same as everywhere else in this ADR; this check is tamper-evident against **accidental**
corruption only, and no stronger claim is made. A derived index carries no catalog (A2) — its OCX-authored
roots are exempt, unchanged filesystem trust.

### F2. Catalog sync — conditional GET first, digest diff

`c/index.json` maps `<ns>/<pkg> → sha256(root)` — it is simply the *catalog part* of the same index, at a
coarser granularity than the per-package roots. `ocx index catalog` reads/syncs this part; `ocx index
update` writes the per-package parts (root + dispatch object). Every flow that touches the catalog issues
an **ETag `If-None-Match` conditional GET first**: a `304 Not Modified` is one round trip with no body (as
cheap as a HEAD, and it hands back a changed body for free when there is one), so the common "nothing
moved" case costs almost nothing — frequently nothing needs pulling at all.

On a `200` (catalog changed), `ocx index catalog` / `ocx index update` against a published index diffs the
remote per-package root digests against the **local** catalog and:

- re-snapshots (via the per-package `ocx index update` path) only the packages whose root digest moved —
  each re-snapshot rewrites the root **and** re-upserts its local catalog entry together (F1), so the copy
  stays internally consistent; a fetched root that disagrees with the catalog entry fetched alongside it
  is benign CDN skew (the root landed ahead of the catalog) and is re-fetched once before the write
  commits;
- for packages in the remote catalog but not yet snapshotted locally, records nothing to verify against
  yet — they are listing rows, materialized only when first `update`d;
- surfaces packages whose remote digest moved but that the user did **not** re-snapshot as "update
  available" (staleness, F1), never an error.

This is the offline-first catalog refresh: one conditional request amortized across the whole catalog, no
per-package polling. The local `c/index.json` (with its `.etag` conditional-GET validator sidecar) is both
the offline catalog-listing source for `ocx index catalog` and the diff basis for the next sync — the
previous file's per-package digests are compared against the freshly-fetched remote map, so no separate
validator store is needed. In the *complete*-copy (full-mirror) case the local `c/index.json` equals the
remote catalog byte-for-byte; in the *partial* case it is the same-grammar subset covering the packages
actually snapshotted.

### F3. Status surfacing (§7.7)

The root's human-governed lane (`status`, `deprecated_message`, `created`, `upstream`,
`superseded_by`, and the per-tag `yanked` marker) is **surfaced, never silently acted on**:

| Field | Surfacing |
|---|---|
| `status: yanked` / per-tag `yanked` marker | Resolve still works if pinned by digest (immutability); a **tag** resolve to a yanked entry emits a stderr warning and, absent an explicit opt-in, is refused (`DataError`) — a yank is a publisher signal, not a delete |
| `status: deprecated` + `deprecated_message` | Warn on resolve; surface the message in `ocx package info` |
| `superseded_by` | Advisory in `info` / resolve diagnostics; never auto-follows (that would override the requested identity — the C-46 identity-binding contract) |

The `yanked` marker is human-governed and survives index regeneration, so it is a stable signal the
client may key on.

### F4. Open interop points (recorded, not built)

- **`desc` blobs** share the `o/sha256/` CAS dir with obs objects but their frozen-contract status is
  **unresolved** index-side. OCX MUST NOT build resolution or verification logic on them until frozen;
  `ocx package info --save-readme/--save-logo` remains registry-metadata-driven for now.
- **Obs platform objects are verbatim OCI** and may carry `os.version` / CPU `features`. At conversion
  to native `Platform`, OCX **warn-drops** unsupported fields (`TryFrom<native::Platform>`,
  established policy, `adr_platform_model_unification.md` D2). Representation fidelity is the index's
  correctness; dropping is OCX's client policy.
- **Obs-digest instability caveat (index-bot sort-key bug).** The index bot's canonical serialization
  omits `os.features`/`features` from its platform sort key, so dual-libc ties can inherit registry
  order and change the obs digest across re-announces (recon caveat; feedback filed). **The OCX client
  MUST NOT assume obs-digest stability across re-announces** until this is fixed index-side: cache obs
  objects by their *observed* digest, never assume a package's obs digest is a stable identity across
  time.

### F5. Routing config — `[registries]` identity + `[mirrors]` policy (owner decision 2026-07-18)

Two orthogonal routing surfaces replace the single index-URL table: `[registries]` declares *what a
namespace is and how it resolves* (identity, project/defaults-owned); `[mirrors]` declares *where traffic
actually goes* (network policy, corp/user-owned). Keeping them in separate tables makes the policy
override-proof for air-gap — a project's registry edits can never undo a corporate mirror.

#### F5a. `[registries."<ns>"] index` — resolution-protocol selector

```toml
[registries."ocx.sh"]
index = "https://index.ocx.sh"    # presence ⇒ ocx-index protocol for this namespace
```

- **Field presence is the kind marker.** A `[registries."<ns>"]` entry that carries `index` resolves via
  the ocx-index protocol (root → obs → `select_best`, Decision C) against that base URL. An entry without
  `index` (or no entry) resolves as **plain OCI**. There is **exactly one** resolution protocol per
  namespace — no index→OCI-tags fallback chain, no runtime format probing. `ocx.sh` flips to index-kind
  through a managed default once `index.ocx.sh` is populated.
- **Plain URL, no compound scheme.** OCX has one index wire dialect, so the URL needs no `<dialect>+`
  prefix — the `index` *field* is the marker (Cargo's own `[registries.NAME] index = "…"` proves a bare
  URL suffices once the table already says what it is). A `<dialect>+https` form is a documented future
  escape hatch, reserved for the day a second wire dialect must share the field (Cargo's `sparse+`
  precedent), not built now (R8).
- **Collision with the existing alias table — folded, not forked.** `[registries."<name>"]` already
  exists as `RegistryConfig`: a `name → hostname` alias for `[registry] default`, consumed by
  `Config::resolved_default_registry` behind the CWE-15 indirection-lock guard (`config.rs:174-193`).
  `index` folds in as a **new optional field on that same table** — an entry may carry a hostname alias,
  an `index` URL, or both. The system-lock loops (`loader.rs:693-718`) and merge branches
  (`config.rs:140-151`) consolidate over the one table: alias-lookup reads the hostname, protocol
  selection reads `index`; they are independent fields on one entry.

#### F5b. `[mirrors."<host>"]` — network policy, keyed by traffic host

```toml
[mirrors]
"ghcr.io" = "https://harbor.corp"                                      # both roles → one host
"index.ocx.sh" = { index = "https://artifactory.corp/ocx-index" }      # index role only
"registry-1.docker.io" = { registry = "http://mirror.local:5000" }     # registry role only
```

- **Keyed by the traffic host, never a namespace.** Index and artifact traffic usually hit *different*
  hosts (an index on `index.ocx.sh`, its artifacts on `ghcr.io`), so a namespace key cannot express the
  split. The key is the host being redirected.
- **Untagged union value.** A **plain URL string** rewrites **both** roles for that host (the common
  single-role host). An **object `{registry?, index?}`** splits per role: `registry` rewrites OCI
  distribution traffic (`/v2`), `index` rewrites index-tree traffic (`/config.json`, `/c`, `/p`). Role
  names, not tech names. An absent field means no rewrite for that role (no fallthrough). The same union
  extends `OCX_MIRRORS` JSON (today a `host → string` object, `env.rs:581-633`): a value is either a
  plain string or a `{"registry"?, "index"?}` object, per host — e.g.
  `{"ghcr.io": "https://harbor.corp", "index.ocx.sh": {"index": "https://artifactory.corp/ocx-index"}}` —
  string values stay valid, objects add the split, and malformed JSON stays fail-closed.
- **Same-host co-serving is fine** — index and registry roles are path-disjoint (`/v2` vs
  `/config.json`,`/c`,`/p`), and the two fields target them independently.

#### F5c. Transport, insecurity, replace semantics, seams

- **http-vs-https is the scheme of the mirror *target*** (containerd pattern, e.g.
  `registry = "http://harbor.corp"`) — no separate protocol flag. `OCX_INSECURE_REGISTRIES` remains the
  escape hatch **only** for plain-http *direct* (unmirrored) hosts (Docker `insecure-registries` pattern).
  `oci://` pointers stay transport-free identity (Helm/ORAS/Flux).
- **Replace semantics, no upstream fallback** (Cargo model — air-gap-correct: a silent egress fallback
  would be a supply-chain surprise). Containerd/podman-style ordered try-then-fallback is a noted future
  knob, not day-one.
- **Seams.** `resolve_mirror_map` (`config/mirror.rs:238-293`) is the single transform; the read-path
  rewrite is `Client::transport_reference` (`client.rs:116-159`) for the OCI role and the index-client
  base-URL mint (`OcxIndex::resolve_base_url`, `index_source.rs:421-454`, today reusing
  `MirrorConfig::parse_url`) for the index role. Both tables merge through the managed-config tier. The
  old single index-URL table (`IndexConfig`) is deleted.

#### Industry Context (why this shape)

Grounded in the cross-ecosystem survey (`research_mirror_config_survey.md`, 11 tools — containers,
language package managers, URL-scheme conventions):

1. **Compound schemes are a two-dialect tax, not a default.** `sparse+` / `git+` fused schemes appear
   only where 2+ wire dialects share one string field (Cargo legacy-git vs sparse-HTTP; pip/npm/Nix
   picking a VCS binary). OCX has one dialect per endpoint kind → plain URLs, the *field name* as kind
   marker (Cargo's `[registries.NAME] index`).
2. **Transport security lives in the scheme of the mirror target**, with a separate escape hatch for
   unmirrored hosts — containerd/Docker/Nix signal plaintext via the target URL scheme;
   `OCX_INSECURE_REGISTRIES` mirrors Docker's `insecure-registries`.
3. **Identity and network-policy are different ownership domains → different tables.** podman
   (`prefix`/`location` vs `registry.mirror`) and Cargo (`[registries.NAME]` vs `replace-with`) both keep
   the *what a source is* declaration override-proof from the *where to reach it* declaration — the map
   for "project-owned identity, corp-owned policy".
4. **`oci://` is a pure kind marker, never transport** (Helm/ORAS/Flux).
5. **Replace vs fallback is a deliberate opposite choice; OCX picks replace** — Cargo's air-gap-correct
   wholesale swap over containerd/Go transparent-cache fallback.

**Open interop point (recorded, not built):** auth for private index mirrors — static-file endpoints
have no OCI token flow; deferred until a real deployment needs it.

---

## Decision G — Old `TagLock` fate (§7.6)

**Decision: delete the read path; no migration, no detection, no delete-on-sight. Regeneration is the
entire story.**

Pre-1.0, refactor-as-if-`TagLock`-never-existed (`feedback_refactor_as_if_never_existed`,
`project_breaking_compat_next_version`): `LocalIndex` reads the verbatim root docs (`p/<ns>/<pkg>.json`); a
stale legacy `{repo}.json` `TagLock` at the old path is simply never read. No transcoder, no version-peek
branch, no "formerly a TagLock" comment. A copy regenerates with `ocx index update` (optionally after
clearing the old directory); a machine's `$OCX_HOME/tags` is orphaned data the user may delete. Adding
delete-on-sight machinery would be exactly the vestige the doctrine forbids.

`tag_lock.rs` (`TagLock`, `Version::V1`, `into_tags`) is deleted with the store rework.

---

## Decision H — Index component model (owner decision 2026-07-18)

**Decision: name the index's parts as separately-consumable components, so a future consumer can take the
local copy and its refresh directly — never the whole resolution chain.** The single wire grammar
(Decision A) lets one local reader serve both provenance kinds, which collapses the old per-format
probing. There is no "tree store" abstraction: there is an index on disk, and `LocalIndex` owns it.

| Component | Role |
|---|---|
| `LocalIndex` | Owns the local index **collection** — one index per source under the home (A1). Reads and writes per-source copies (roots, catalog, dispatch-object CAS) with verbatim bytes + recompute-verify (A4); serves the local half of the chain. Both provenance kinds go through it — the only divergence is two `if`s: catalog from `c/index.json` (published) vs directory enumeration (derived), and obs-object vs image-index decode. The atomic filesystem mechanics (tempfile+rename CAS, the `write_object` self-heal write, `file_structure/snapshot_store.rs`) are an internal detail of `LocalIndex`, not a headline component. |
| `OcxIndex` | Remote client of a **published** ocx-index (root → obs → `select_best`; Decision C). |
| `OciIndex` | Remote client that **derives** an index from a plain OCI registry's tags API. |
| `IndexSync` | Keeps local copies current: conditional-GET catalog digest-diff for published indices (F2), per-repo tag refresh for derived ones. Auto-refresh is a **policy seam** here — no daemon now. |
| `ChainedIndex` | `LocalIndex` ▸ **exactly one** remote per namespace. |

- **`ChainedIndex` = `LocalIndex` ▸ exactly one remote per namespace.** The remote is `OcxIndex` or
  `OciIndex`, chosen by config (F5a `index` field presence), never probed. The former index→OCI-tags
  fallback chain and its `FormatProbe` / `OnceCell` negative caching (the `ensure_usable` gating on
  `resolve_root` / `sync_catalog`) dissolve — config states the kind, so there is nothing to probe.
- **Separately consumable.** A future consumer — a search/TUI, `ocx-mcp`, an alternate index root — takes
  `LocalIndex` + `IndexSync` directly, for read-only browsing and bulk refresh, without dragging in the
  remote-resolution chain. That is the reason these are named components rather than private internals of
  the resolver.

---

## Non-Goals — recorded threats & deferrals

- **Signing (deferred, future ADR).** Record the mix-and-match threat verbatim (D-E): *an unsigned
  index combined with signed manifests permits resolve-time platform-set tampering* — a party who can
  rewrite the (unsigned) index can substitute the platform→digest set an obs object advertises, steering
  a host to a different (but individually validly-signed) manifest than the publisher intended. Closing
  this needs index/obs signing, specified when the signing ADR is written.
- **`repository`-change on already-installed packages = TOFU** (trust-on-first-use), accepted stance
  pending the signing ADR.
- **Index-repo announce/reconcile internals, M-1 gating, public-index seeding** — see Scope.

---

## Data Model (● = frozen wire contract, from the authoritative index structure)

```
index.ocx.sh served tree  (● = frozen; the local index-source subtree is a verbatim copy of this)
/
├── config.json                       ● {"format_version": 1}
├── c/index.json                      ● {"<ns>/<pkg>": "sha256:<root-digest>", ...}
└── p/<ns>/
    ├── <pkg>.json                    ● root: name, repository (oci://…), owners, status,
    │                                        deprecated_message, created, upstream, superseded_by,
    │                                        desc→CAS, tags{ "<tag>": {content, observed, yanked} }
    └── <pkg>/o/sha256/
        ├── <hex>.json                ● observation object: platforms[]{ platform{…OCI…}, digest }
        └── <hex>.md|.svg|.png          desc blobs — frozen-status UNRESOLVED (open, F4)

local index collection (home: --index ▸ OCX_INDEX ▸ $OCX_HOME/index) — one index per <source>
<home>/<source>/                                  the hosted grammar verbatim (A2)
├── config.json                       published index only — verbatim copy of the served config
├── c/index.json (+ .etag)            published index only — verbatim/partial catalog + conditional-GET validator
└── p/<ns>/
    ├── <pkg>.json                    root doc: repository (oci://<physical>), status/superseded_by/…,
    │                                    tags{ "<tag>": { content, observed, yanked } }
    │                                    (published: copied bytes; derived: OCX-authored,
    │                                     no config.json/c/, catalog = directory enumeration)
    └── <pkg>/o/sha256/
        └── <hex>.json                DISPATCH objects only — obs object (published) or image
                                        index (derived); sha256(bytes) == <hex> (A4). Leaf
                                        platform manifests are NEVER here — they are content in the
                                        machine-global blob store, fetched on demand (A3, B2).
```

Grammar for a source-qualified logical reference (resolution input):

```ebnf
logical-ref   = index-ns "/" namespace "/" package [ ":" tag | "@" digest ] ;
index-ns      = "ocx.sh" ;                      (* the single baked known-index key *)
repository    = scheme "://" host "/" path ;    (* physical, transport-only, never a storage key *)
scheme        = "oci" ;                          (* type marker + wire contract, one-way door, C3;
                                                    the only scheme — source kind comes from the
                                                    <source> path segment + config, not a scheme *)
```

---

## Consequences

**Positive**

- **#215 closed.** A shipped index copy resolves a *version choice* **offline and deterministically**:
  the root docs + digest-verified dispatch objects yield the exact platform-manifest leaf digest and
  physical location with zero network and zero machine-global dependence (A3). This is version resolution,
  not lock resolution — locks were already index-free; the win is that free version *choice* (a
  devcontainer parameter, a Gradle property, a CLI tag) no longer needs the network. The old dependence on
  a machine-global blob store for the dispatch object is gone.
- **~6× smaller index copy.** A copy holds only dispatch objects, not the manifest chain (A3): a typical
  package drops from ~6 objects to 1 (e.g. lychee 6 → 1). `ocx index update` also does *less* work — one
  dispatch-object write per tag, no child-walk.
- **Verifiable + copy-a-mirror.** Every object hashes to its filename (A4); a published-index copy is a
  verbatim site copy that verifies against itself — objects by filename, roots against their
  `c/index.json` catalog digest (A2, F1); re-push replays byte-identical content.
- **Migration-proof identity.** Logical keying, made structural by the path-named identity (A2/C), means a
  `repository` migration never orphans a store or drifts a GC root; any committed reference to a package
  survives it (logical id + content digest, never a physical host).
- **Uniform lock unit.** Platform-manifest digest across lock/pin/index (D), with the dispatch-object
  exemption written down.
- **No new GC surface.** The local index stays outside reachability (B1); `ocx clean` cannot eat a shipped
  copy's data.

**Negative / Accepted trade-offs**

- **Cold-store content fetch needs network.** A clean clone resolves the dispatch offline but must fetch
  the leaf manifest and layers from the registry — **zero functional loss**, since a cold store needs
  that same network for the layer download anyway (A3). Installed-tool offline exec is unaffected (the
  leaf manifest is cached at install time, B2).
- **A dispatch object may be duplicated** across a local `o/` and `$OCX_HOME/blobs` (an installed
  package's parsed image index). Accepted: distinct lifecycles (B2); collapsing them recouples the index
  copy to GC (R2).

**Risks**

| Risk | Mitigation |
|---|---|
| Object digest drift (bytes ≠ filename) | A4 recompute-and-verify makes this a hard write-time error, not a latent corruption |
| Obs-digest instability across re-announces (index-bot sort-key bug) | F4: client never assumes obs-digest stability until fixed index-side |
| Unsigned-index tampering | Non-Goals: recorded threat; TOFU accepted pending signing ADR |
| Building on unfrozen `desc` blobs | F4: explicitly not built on until frozen |

---

## Rejected Alternatives

### R1 — Platform→digest map authored into the root doc

Author, per tag, the resolved `platform → leaf-digest` map directly into the root doc (skip the object
CAS, resolve straight from the root). **Rejected on three counts:** (i) it is an **invented
representation** — a shape OCX mints rather than one it copies verbatim (index source) or holds as a
verbatim dispatch object (OCI source), so nothing hashes to a source-verifiable digest (violates DR2);
(ii) it is functionally the `index.ocx.sh` **observation object**, a pure function of the OCI index —
copy it verbatim for interop, never re-mint it (D-B); (iii) it drags in **variant-type upgrade machinery**
— canonical-grammar validation, dedup, and same-`Platform` collision handling on every write (the very
apparatus `adr_platform_model_unification.md` D3 confines to `ocx.lock`) — for zero gain over holding the
verbatim dispatch object and running `select_best` at resolve time.

### R2 — Snapshot documents in the blob store + symlinks (≈ status quo)

Keep manifest bytes in the machine-global `$OCX_HOME/blobs` and give a local index copy a tree of
symlinks (or tag files) pointing at them. **Rejected:** this is essentially today's layout, and it
(i) **loses self-containment** — a shipped copy's symlinks dangle into a blob store the target machine
does not have (the #215 defect); (ii) **couples the copy to the GC sweep** — if the copy's targets live in
the GC-walked blob store, a machine-local `ocx clean` can collect bytes the copy depends on (DR6
violation); (iii) symlinked data is not portable across machines/OSes (Windows junctions, relative-path
fragility). A self-contained verbatim CAS under the index root avoids all three.

### R3 — Keep `OCX_INDEX` redirecting the `TagStore` only (status quo)

Leave `context.rs:184-192` as-is. **Rejected:** it is the root cause — the local copy is tag-only and not
self-contained. Redirecting the whole local index collection (A1) is the fix.

### R4 — Store `index.ocx.sh` obs data as a locally-computed OCI image index

Convert each obs object into a synthetic OCI image index and store it in the OCI-registry snapshot
format, unifying the two source codecs. **Rejected (D-B):** it invents a digest namespace (the
synthetic index has no registry-verifiable digest), and it re-introduces the image-index hop that C
deliberately bypasses. Obs objects are cached verbatim in their native format; they are immutable, so
"cache forever, verify once" is both simpler and correct.

### R5 — Physical-keyed storage / lock (`design_spec_registry_indirection.md`)

Key storage paths and `ocx.lock` on `.physical()`. **Rejected (superseded by C):** the `repository`
pointer is volatile by design; physical keying breaks committed references (locks, pins) on any storage
migration and opens the `slugify`-drift GC-orphan hazard. Logical identity + content digests survive
migration.

### R6 — Bring the local index into the GC reachability graph

Give the local index a new `CasTier` variant, join it into `list_all()`, and wire root/edge reachability
(the option sketched in facts §3). **Rejected (decided inline in B1):** the local index is
user/deployment-managed data with a lifecycle independent of installed content; walking it in `ocx clean`
risks collecting a shipped copy's data (DR6) and buys nothing — the store is never a GC root and holds no
forward-refs. Keeping it outside the graph (B1) is the cheaper and safer half of the "in vs out" choice.

### R7 — Store the full manifest chain in the local index

Snapshot the whole image-index → platform-manifest chain into `o/` so a cold store needs no network for
content. **Rejected (A3):** a leaf manifest is immutable, content-addressed content — fetchable on demand
from any registry and CAS-verified — not a volatility pin. Storing it bloats the copy ~6× for **zero
determinism gain**: the dispatch (what a re-push can change) is already pinned by the stored dispatch
object, and the cold-store fetch it would avoid is the same network the layer download needs anyway. The
local index pins volatility; content stays in the blob store (B2).

### R8 — Compound `index+https` scheme for the index-kind marker

Mark a namespace's ocx-index protocol with a fused `index+https://…` URL scheme instead of the `index`
*field* on `[registries]`. **Rejected (F5, `research_mirror_config_survey.md`):** compound schemes are a
two-dialect tax — they earn their keep only where 2+ wire dialects share one string field (Cargo
`sparse+`, pip/npm/Nix VCS selection). OCX has one index dialect, so the field name is the kind marker
already (Cargo's own `[registries.NAME] index = "…"` uses a bare URL). `<dialect>+https` is reserved as a
future escape hatch, not a day-one shape.

---

## Validation (contract, not implementation)

- [ ] A **shipped** index copy resolves a *version choice* (root doc → dispatch object → `select_best` →
      platform-manifest leaf digest + physical location) on a clean machine with **no network and no
      `$OCX_HOME/blobs`**; the leaf manifest + layers then fetch from the registry (content on demand) and
      verify OCI CAS (#215 determinism, A3). No `ocx.lock` is involved — locks resolve index-free.
- [ ] Every `p/<ns>/<pkg>/o/<algo>/<hex>.json` file satisfies `sha256(bytes) == <hex>` (A4); a
      byte-tampered object fails the write/read with `DataError`, never silently loads.
- [ ] `ocx index update <pkg>` writes the dispatch object, then the root `p/<ns>/<pkg>.json`, then the
      package's `c/index.json` entry, in that fixed order with each step an idempotent atomic-rename write
      (F1); a crash interrupted between any two steps recovers on the next read or update — a root/catalog
      digest mismatch is re-derived from the root and the catalog rewritten, never rejected, logged at
      info/debug. Only actual corruption (an unparseable root, a dispatch object whose bytes disagree with
      its own `o/` filename, a failed `repository` cross-check) still hard-fails with `DataError`; a
      catalog sync finding a *different remote* digest for an already-snapshotted package reports "update
      available" and never errors (inconsistency-vs-staleness, F1/F2).
- [ ] An index-source subtree written by `ocx index update` equals a `rsync`/`wget --mirror` copy of the
      site **byte-for-byte**, and verifies against `c/index.json` + `o/` filenames alone — no OCX-minted
      metadata (copy-a-mirror, A2).
- [ ] `ocx index update` against an OCI registry writes the verbatim **image index** (dispatch) into `o/`
      and **no** child platform manifests (A3); a single-platform tag writes nothing to `o/` and records
      the manifest digest as `content`. A committed multi-platform package holds one dispatch object, a
      single-platform one holds none (~6× shrink).
- [ ] A tag whose `content` is **absent** from `o/` resolves the leaf via registry fetch-by-digest; if
      the fetched bytes decode as an image index the snapshot **self-heals** (writes it into `o/`, then
      continues dispatch), and if they decode as a manifest it takes the leaf path (A3).
- [ ] `--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index` home resolution over the local **collection** (one index
      per source); `$OCX_HOME/tags` is no longer read or written; the direnv export never sets `OCX_INDEX`
      (A1).
- [ ] `ocx clean` never inspects or collects any file under the index home (local index outside the GC
      graph, B1).
- [ ] Installed-package `refs/blobs/` retention (`add_index_retention_edges`) still keeps a live platform
      manifest's parent image index alive (unchanged); an index.ocx.sh flat-leaf blob adds no retention
      edge (`ImageIndex` guard no-match).
- [ ] A `repository`-migrated package (same logical id, new physical host) resolves and stores under the
      **logical** path; a committed reference from before the migration still resolves.
- [ ] `index.ocx.sh` resolve: root → obs (verify sha256) → `select_best(host, platforms)` → physical
      manifest from `repository` (verify OCI CAS); the OCI image-index hop is never taken.
- [ ] Obs object cached forever; the root is copy-first under Default (never auto-refreshed) and
      re-fetched **only** on `ocx index update`/`--remote`, with `observed` bumped on refresh. Catalog
      flows issue an **ETag `If-None-Match` conditional GET first** (`304` = one round trip, no body); on
      change the per-package digest diff re-snapshots only moved-root packages. `ocx index catalog` lists
      a published index from `c/index.json` and a derived index from directory enumeration — **per-source**,
      no global merged catalog.
- [ ] A tag resolving to a `yanked` entry warns and refuses (absent explicit opt-in) — surfaced
      **offline** from the committed root doc; a digest-pinned resolve of the same content still succeeds.
- [ ] `ocx package push --canonical-tag` (default) pushes a `sha256.<hex>` tag per platform manifest,
      keeping on-demand-fetchable leaves registry-GC-reachable (E); `--no-canonical-tag` suppresses it;
      neither affects index resolution.
- [ ] A stale legacy `TagLock` `{repo}.json` at the old path is ignored (never read, no error); `ocx
      index update` regenerates the root docs.
- [ ] `[registries."<ns>"] index` presence selects the ocx-index protocol, absence selects plain OCI (no
      probing); a `[mirrors."<host>"]` plain string rewrites both roles, an object `{registry?, index?}`
      splits per role; `OCX_MIRRORS` string and object values both parse and malformed JSON fails closed;
      with an index-role mirror set, every root/obs/`c/index.json` fetch hits the override base URL and
      none the default host (replace semantics), obs sha256 verify still applying to mirrored bytes (F5).

## Links

- [`handover_index_indirection.md`](./handover_index_indirection.md) — locked decisions D-A…D-E, source
  of this ADR (work item #2/#3)
- [`adr_platform_model_unification.md`](./adr_platform_model_unification.md) — `is_compatible` /
  `select_best` / canonical grammar / `ocx.lock` V3 (lock unit, D3) — the dependency this ADR builds on
- [`adr_oci_registry_mirror.md`](./adr_oci_registry_mirror.md) — the `[mirrors]` transport-seam
  precedent copied by Decision C
- [`research_mirror_config_survey.md`](./research_mirror_config_survey.md) — 11-tool cross-ecosystem
  mirror/redirect-config survey grounding Decision F5 (Industry Context)
- [`adr_index_routing_semantics.md`](./adr_index_routing_semantics.md) — `IndexOperation::{Query,
  Resolve}`; ChainMode routing the local index plugs into
- [`adr_project_gc_symlink_ledger.md`](./adr_project_gc_symlink_ledger.md) — GC roots the local index must
  stay outside of (B1)
- `design_spec_registry_indirection.md` — **superseded** stale client spec (physical-keyed, overturned
  by C)
- ocx-sh/index: `adr_locked_observation_index_format.md` (D1 lock unit), `bot/CONTRACTS.md` (canonical
  serialization + the sort-key caveat), `schema/{root,observation-object}.schema.json`
- [issue #215](https://github.com/ocx-sh/ocx/issues/215), [issue #212](https://github.com/ocx-sh/ocx/issues/212)
- [OCI image-index spec v1.1.1](https://github.com/opencontainers/image-spec/blob/v1.1.1/image-index.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-18 | architect (opus) | Initial ADR specifying the index-track decisions A–G from `handover_index_indirection.md` (D-A…D-E). §7.1 home question resolved by code verification: `OCX_INDEX`/`--index` today redirects only the `TagStore` (`context.rs:184-192`), so the committed `.ocx/index/` is tag-only and not self-contained, and `ocx index update` writes no manifest blobs (`command/index_update.rs:19`) — motivating a replacement self-contained `SnapshotStore` (A1) with verbatim, digest-verified object CAS (A3). Snapshot kept outside GC (B1); `add_index_retention_edges` retained (B2); logical-identity keying with the mirror-seam precedent (C); lock-unit doctrine + snapshot exemption written explicitly (D); canonical tags default-on (E); index.ocx.sh two-hop consumption build-gated on M-1 with frozen ● shapes only, obs-digest-instability caveat recorded (F); old `TagLock` deleted with no migration (G). Rejected: platform→digest map in `tags.json` (R1), snapshot-in-blob-store+symlinks (R2), physical-keyed storage (R5). |
| 2026-07-18 | orchestrator (fable) | F build gate lifted per owner decision 2026-07-17 ("Full F now"): client built against frozen ● shapes with static-file HTTP fixture acceptance; live M-1 smoke post-merge. Added F5 "Index mirroring" — `[indices."<ns>"] url` global-config table for the static-file half (logical-ns key, replace semantics, managed-tier merge, resolution at index-client seam; private-mirror auth recorded open point); OCI half already covered by C1 `mirror_map`. Validation item added for `[indices]` override. |
| 2026-07-18 | architect (opus) | Adversarial-panel revision. Default machine-local snapshot home relocated to `$OCX_HOME/state/registry-index/` to honor locked D-B (state-vs-cache naming); A1 table, Data Model, Validation aligned. Decision D exemption scoped per source kind (image-index digest for OCI-registry source; observation-object digest with manifest-digest-pinned leaves for `index.ocx.sh` source) and corollary corrected. F1 root reconciled with Invariant 1 (snapshot-first under Default; live re-fetch only on `ocx index update`/`--remote`). Context section corrected — `OCX_INDEX` presently unwired (taskfile override removed in `3d13af27`), `.ocx/index/` not git-tracked, restore noted as an A1 prerequisite; footnote arrow direction and `file_structure.rs:67-69` citation fixed. `ocx://` local-snapshot source marker added to the grammar and separated from the frozen index-side `oci://` in C3. DR6 reworded to CAS write/verify primitives (A2 diverges from `cas_shard_path` sharding). A3 cites `fetch_manifest_raw_bytes` prior art (recompute-verify is the delta). A2 summary `p/` segment fixed. F2 states `c/index.json` persists at `{index-home}/c/index.json`. Added R6 (GC-inclusion rejected). |
| 2026-07-18 | architect (opus) | Owner-decision design-set amendment (as-if-never-existed; supersedes the `tags.json` re-encoding, `[indices]` table, transitive manifest snapshotting, and `FormatProbe` in place). **A** reworked: the local tree adopts the hosted wire grammar verbatim (`<source>/{config.json,c/,p/<repo>.json,p/<repo>/o/<algo>/<hex>.json}`) — index sources are self-verifying site copies (copy-a-mirror), OCI sources OCX-authored in the same grammar; per-source catalog; logical identity from the path. **Headline (A3):** the object store holds **dispatch objects only** (obs objects / image indexes) — leaf manifests are never copied; absence-from-`o/` ⇒ manifest, self-heal on incomplete snapshot; `persist_manifest_chain` recursion deleted. **B2** reframed (leaf manifests + layers live in the blob store, fetched on demand). **D** exemption collapsed to one story (the `content` pointer names a verified dispatch object; leaves are doctrine-correct manifest digests fetched on demand). **E** gains the registry-GC-safety tie (canonical tags keep on-demand leaves reachable). **F5** replaces `[indices]` with `[registries."<ns>"] index` (identity/protocol selector, folded into the existing `RegistryConfig` alias table) + `[mirrors."<host>"]` traffic-host policy (untagged `{registry?,index?}` union, `OCX_MIRRORS` extended, replace semantics), with an Industry Context subsection citing `research_mirror_config_survey.md`. **New Decision H** — managed component model (`IndexTree`/`MirrorIndex`/`OcxIndex`/`OciIndex`/`IndexSync`; `ChainedIndex` = one remote per namespace; `FormatProbe` dissolved). **A1** removes the ambient direnv `OCX_INDEX` export. DR1/Consequences reframed (deterministic offline *resolution*, ~6× smaller committed index, cold-store content fetch = zero functional loss). Grammar `ocx://` marker removed. Added R7 (commit-full-chain rejected), R8 (compound `index+https` rejected). Validation + Data Model rewritten. |
| 2026-07-18 | architect (opus) | **Conceptual-spine rewrite; lock coupling stripped (owner: "the OCX index is just a directory where a local copy of an index is stored… this has nothing to do with the ocx.lock").** Context reopened on the spine — **one** index format, differing only by *location* (hosted/mirrored/shipped/local), *provenance* (published = copied vs derived = OCX-authored from OCI tags), and *completeness* (partial per-package vs the degenerate full-mirror); the local home is a **collection**, one index per source. The index now serves **tag/version resolution only** — locks are index-free (they read the pinned leaf digest directly, `adr_platform_model_unification.md` D3); redirection (`--index`/`OCX_INDEX`) exists so a **shipped** copy resolves free version *choice* (devcontainer parameter, Gradle property, CLI tag) deterministically offline (#215 reframed). Stripped all lock/toolchain coupling from Context/DR1/DR3/C/D/G/Consequences/Validation (D kept as shared keying/verification discipline, not a resolution dependency). Default home `$OCX_HOME/state/registry-index` → **`$OCX_HOME/index`** (first-class store; `LocalIndex` home field `snapshot` → `index`); repo path de-architected (shipped copy lives anywhere, `--index`/`OCX_INDEX`; conventionally `.ocx/`, never required). **Decision H recast** to `LocalIndex`/`OcxIndex`/`OciIndex`/`IndexSync`/`ChainedIndex` — `IndexTree`/`MirrorIndex`/"tree store" abstraction killed (there is an index on disk; the `SnapshotStore` fs-layer is a `LocalIndex` internal). **F1/F2 rewritten** for partial snapshots: `ocx index update <pkg>` upserts the package's catalog entry atomically with its root (root always matches its own entry by construction) → read-time mismatch = tamper/corruption (`DataError`), remote-catalog drift = staleness ("update available"), never conflated; catalog flows issue an ETag `If-None-Match` conditional GET first (304 = one round trip, no body). Kept: dispatch-only `o/` + self-heal, the wire grammar, F5 `[registries]`/`[mirrors]` + Industry Context, R7/R8, E registry-GC tie. |
| 2026-07-18 | builder (sonnet) | F1 crash-safety correction: the three-step upsert (dispatch object → root atomic-rename → catalog atomic-rename) was never one operation, so a crash between steps was previously undefined; rewritten as a fixed, idempotent step order where any interruption recovers on next read/update. Catalog entries reclassified as *derived* from the root (`sha256(root bytes)`), so a root/catalog mismatch is an inconsistency recovered by re-derivation and logged at info/debug, not a hard `DataError` — `DataError` narrowed to genuine corruption (unparseable root, dispatch-object digest failure, `repository` cross-check failure). Trust framing corrected: the check is tamper-evident against accidental corruption only, not adversarial substitution (root and catalog share one trust domain). Staleness semantics (remote catalog drift = "update available") unchanged. Validation item reworded to match. Added **Rules Sync** metadata field noting `subsystem-oci.md` / `subsystem-file-structure.md` / `product-context.md` storage layout still describe the superseded design pending the docs work package. |
| 2026-07-19 | builder (sonnet) | **Flat blob CAS deleted from the local index home — B2 tightened.** `SnapshotStore` (renamed `IndexStore`, `file_structure/index_store.rs`) held two address grammars under one root: the wire-grammar collection (A2) plus an additive flat opaque-blob CAS (`object_path`/`write_object`/`object_exists`) for non-manifest content. The second grammar is deleted — it never had a production caller. Config blobs and any other non-manifest content live solely in the machine-global blob store (`$OCX_HOME/blobs`), GC-tracked via `refs/blobs/` for as long as a package installing them stays live (B1/B2 unchanged: the index home itself stays outside the GC graph, only the blob store is walked). `LocalIndex::fetch_blob` is now an `Ok(None)` stub; `ChainedIndex::fetch_blob` does the real work — cache-first digest-verified read against its attached `BlobStore` (`content_store`), a corrupt cached entry removed via the new `BlobStore::remove_blob` before falling through to the source walk, and a digest-verified write-through after a successful source fetch (CWE-345 class covered on both read and write directions). **Accepted cut:** this deliberately removes the GC-immune metadata-cache behavior an earlier revision of this ADR implied — offline repeat-inspection of a package that was cached but never installed no longer survives `ocx clean`, because that cache lived only in the now-deleted flat CAS. Nothing in Decision A–H depended on that behavior; the dispatch-object CAS (A3) is unaffected. **Wire digest fields retyped.** `RootTag::content`, `ObservationPlatform::digest`, and `DerivedTag::content` are now `oci::Digest` (were `String`) — exact-wire serde, no shape change. A malformed digest now fails the whole root-document parse: published-index roots surface `MalformedRootDocument`, derived (OCI-source) roots surface `MalformedIndexDocument`; a derived root's "start fresh" recovery path drops sibling tags on such a failure (accepted, test-locked — a malformed tag poisons the whole per-package root, not just itself). `CatalogIndex` stays `BTreeMap<String, String>` — unretyped by design (F1's blast-radius guarantee: one bad `c/index.json` entry never fails the whole catalog read, only that entry's own re-derivation). New failure-mode tests reference this amendment. Docs sync: `subsystem-file-structure.md` (`SnapshotStore`→`IndexStore` rename, flat-CAS purpose text dropped, "Two halves" paragraph rewritten), `arch-principles.md` (composite-root table), `subsystem-oci.md` (Absent-leaf recovery extended one sentence for the shared `fetch_blob` seam). |
