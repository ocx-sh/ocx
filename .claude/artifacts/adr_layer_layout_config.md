# ADR: Decouple layer extraction from package strip/layout config

## Metadata

**Status:** Accepted
**Date:** 2026-07-02
**Deciders:** Michael Herwig (owner), Architect
**Beads Issue:** N/A (create GitHub issue on approval)
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust core, no new deps)
**Domain Tags:** data, infrastructure
**Supersedes:** N/A
**Superseded By:** N/A

## Context

`strip_components` is a **package-wide** field on `Bundle` metadata
(`crates/ocx_lib/src/package/metadata/bundle.rs:47`), but it is **applied at
layer-extraction time** into the shared, content-addressed layer store.

Extraction path applies strip while writing into
`layers/{registry}/{digest}/content/`:
- registry: `oci/client.rs:535` (read) → `client.rs:610/630/646` (apply per codec)
- local: `pull_local.rs:480` (read) → `pull_local.rs:486` (apply)
- skip logic: `archive/tar.rs:185`, `archive/zip.rs:240`

The layer store is keyed by **registry + compressed blob digest only**
(`file_structure/layer_store.rs:62`) — not by strip value, not by package.
Layers are shared across packages (`LayerRef::Digest` reuse, cache fast-path at
`pull.rs:757`). The hardlink walker `assemble_from_layers`
(`utility/fs/assemble.rs:193`, call site `pull.rs:401-408`) mirrors that content
**verbatim** into each package — it never re-strips.

### The bug (confirmed)

A caller-supplied transform (`strip_components`) is baked into a
content-addressed store whose key does not include that transform. Consequences:

1. **Shared-state corruption.** Package A (`strip=1`) and package B (`strip=0`)
   that reference the **same blob digest** race on one `layers/{digest}/content/`.
   `finalize_layer_dir` is first-writer-wins (`layer_staging.rs:45`). Whoever
   extracts first defines the stripped tree; the other silently gets the wrong
   layout.
2. **Invariant violation.** `layers/{digest}/content/` is supposed to be the
   faithful extraction of a blob (dedup unit). Stripping makes it a function of
   package config, not of blob bytes.
3. **Multi-layer packages can't vary strip per layer** — one `Option<u8>` for
   all layers. A base layer and a vendored layer needing different strip cannot
   both be satisfied.

This ADR splits the fix into a **must-do bug fix** (Part 1) and an
**additive feature** (Part 2). Part 2 is the durable wire-format decision that
needs sign-off.

## Decision Drivers

- **Correctness:** content-addressed layer store must be faithful to blob bytes;
  no per-package mutation of shared state.
- **Backward compatibility:** metadata + OCI manifest must stay compatible for
  already-published packages (CLAUDE.md hard constraint).
- **No migration prose / no compat shims** (project convention: pre-1.0 breaks
  just break, but published wire format is the exception that must not break).
- **KISS / YAGNI:** ship the minimal fix; add per-layer expressiveness only with
  a real multi-layer use case.
- **Reversibility:** Part 1 is fully reversible (internal timing). Part 2 is a
  one-way door (published wire format).

## Industry Context & Research

**Key insight:** Content-addressed stores must materialize *faithful* content;
all caller-specific transforms happen at the **materialization/checkout** step,
never at ingest. This is the shared invariant behind:

- **OCI image layers** — layers are extracted verbatim into the overlay; no
  per-consumer path rewriting at unpack. `strip-components` is a `tar(1)`
  convenience, not a layer property.
- **Nix store** — store paths are immutable and content-addressed; relocation /
  wrapping happens at profile build, not on the store object.
- **Bazel** — the CAS holds raw artifacts; the sandbox/runfiles tree applies
  per-target layout on top.

OCX's own three-tier rationale (`subsystem-file-structure.md`: "layers/ …
dedup unit for multi-layer packages") already commits to this invariant. The
bug is a violation of a principle the architecture already states.

**Research artifact:** N/A (well-established invariant; no novel tech choice)
**Trending approaches:** per-layer descriptor annotations for transport-layer
metadata (OCI-native; `annotations` already on every `Descriptor`).

## Considered Options

Two independent decisions. Part 1 has one clear fix; Part 2 chooses the config home.

### Part 1 — When/where to strip (bug fix)

#### Option 1A: Extract verbatim, apply package-wide strip at assemble time — **recommended**

**Description:** Remove `strip_components` from the layer-extraction path
(extract with `strip=0` → faithful blob tree in `layers/{digest}/content/`).
Apply the existing package-wide `Bundle.strip_components` inside
`assemble_from_layers`, transforming dest paths as it hardlinks into
`packages/.../content/`. Wire format unchanged.

| Pros | Cons |
|------|------|
| Restores content-addressed invariant; shared layer store faithful | Layer store holds un-stripped dirs (marginally more inode/dir entries) |
| Fixes reuse corruption + first-writer race with no wire change | Strip logic moves from tar/zip extractor to assemble walker |
| Zero migration; published packages keep exact semantics | — |
| Fully reversible (internal timing only) | — |

#### Option 1B: Key the layer store by (digest, strip)

**Description:** Keep stripping at extraction; add strip to the layer store path
so different strips get different extracted trees.

| Pros | Cons |
|------|------|
| Minimal code move | Re-extracts same blob per distinct strip — defeats dedup unit |
| — | Strip is not the only future transform; key grows unboundedly |
| — | Wastes disk; contradicts "layers = dedup unit" rationale |

### Part 2 — Where per-layer layout config lives (additive feature)

Per-layer `strip` + optional output `prefix` (layer → subdir within package).

#### Option 2A: Manifest layer-descriptor annotations — **recommended**

**Description:** Carry per-layer config in each manifest layer descriptor's
`annotations` (`oci::Descriptor.annotations`, already present, always `None`
today — `manifest_builder.rs`). Keys e.g. `sh.ocx.layer.strip-components`,
`sh.ocx.layer.prefix`. Assembler reads them while iterating manifest layers.
Fallback chain: per-layer annotation → package-wide `Bundle.strip_components`
→ `0`.

| Pros | Cons |
|------|------|
| Config attaches to the *reference* (package's view), so it decouples from the shared blob automatically | Annotations are stringly-typed (`BTreeMap<String,String>`) — no schemars validation |
| Layers already live only in the manifest, not metadata — natural home | Two config surfaces (metadata blob + manifest annotations) |
| No index-alignment coupling (assembler already walks manifest layers) | Parse/validate needed at read boundary |
| OCI-native; inspectable by oras/docker | — |
| Same blob reused by two packages can carry different layout per manifest | — |

#### Option 2B: `layers: [LayerConfig]` array on `Bundle` metadata

**Description:** Add a typed, schemars-validated array to `Bundle`, index-aligned
with the manifest `layers[]`.

| Pros | Cons |
|------|------|
| Typed, schema-validated, single "config" surface | Must stay index-aligned with manifest layer order — fragile; layers supplied as separate `&[LayerRef]` CLI args |
| — | Shadows the transport layer list inside the semantic metadata blob (mixes concerns) |
| — | Config is on metadata (package-global blob) yet must mirror per-layer transport order |

#### Option 2C: Do nothing beyond Part 1 (defer per-layer entirely)

**Description:** Ship Part 1; keep strip package-wide. Revisit if/when a concrete
multi-layer-different-strip or output-subdir need appears.

| Pros | Cons |
|------|------|
| Smallest surface; YAGNI-honest | Multi-layer packages still can't vary strip per layer or place layers in subtrees |
| No wire decision to lock in now | — |

## Decision Outcome

**Chosen:** **Part 1 → Option 1A** and **Part 2 → Option 2A**, both built now
(owner decision 2026-07-02).

**Rationale:**

- **Part 1 (1A)** is the actual root-cause fix: the store must be faithful, and
  the only correct place for a caller-supplied transform is at materialization
  (assemble), per-package. It fixes the reported corruption and the race with no
  wire change and no migration — the package-wide `strip_components` keeps working
  exactly as published today, just applied one step later.
- **Part 2 (2A)** wins the wire decision because layers are *already* a
  manifest-only concern; putting layout config on the descriptor (a) keeps the
  config attached to the package's *reference* to a blob — which is exactly the
  granularity that decouples it from the shared blob — and (b) avoids the
  index-alignment fragility of 2B. The stringly-typed con is contained with a
  typed `LayerLayout` parsed/validated at the read boundary.
- Both parts ship together: Part 1 restores the invariant; Part 2 layers the
  per-layer expressiveness (per-layer strip + output prefix) on top. Sequence
  Part 1 → Part 2 within the work so the bug fix lands first and Part 2 builds on
  the assemble-time transform introduced by Part 1.

### Consequences

**Positive:**
- `layers/{digest}/content/` becomes a faithful, reusable extraction (invariant restored).
- Reuse of one blob by packages with different strip is correct.
- Clean separation: `Bundle` metadata = layer-agnostic package properties
  (env, deps, entrypoints); manifest annotations = per-layer placement.

**Negative:**
- Assemble walker gains path-transform responsibility (strip; later prefix).
- If Part 2 ships, a second config surface (annotations) exists alongside metadata.

**Risks:**
- **Overlap after transform (Part 2):** per-layer strip/prefix can make two
  layers collide in the package tree. *Mitigation:* run per-layer transforms
  **before** the existing overlap detection in the multi-layer merge
  (`assemble.rs:197-338`); reject collisions with a clear error.
- **Path traversal via `prefix` (Part 2):** *Mitigation:* validate `prefix` is a
  relative, lexically-normalized, non-escaping path using
  `utility/fs/path.rs::{lexical_normalize, escapes_root}` at publish and at read.
  Hand to `/security-auditor` before merge of Part 2.
- **Disk in layer store (Part 1):** un-stripped trees slightly larger. Negligible.

## Technical Details

### Architecture (data flow)

```
BEFORE (bug):
  blob(D) --extract+STRIP--> layers/D/content/   <-- strip baked into shared store
                                   |  (verbatim)
                                   v
                             packages/{A,B}/content/   <-- both inherit same strip

AFTER Part 1:
  blob(D) --extract verbatim--> layers/D/content/   <-- faithful, shared
                                   |
              +--------------------+--------------------+
              v (strip per A's cfg)                     v (strip per B's cfg)
        packages/A/content/                       packages/B/content/

AFTER Part 2 (per layer L in manifest):
        layout = descriptor(L).annotations
                 ?? Bundle.strip_components ?? 0
        assemble: dest = prefix / strip(relpath, layout.strip)
```

### API Contract (Part 2, if built)

Per-layer annotation keys on `oci::Descriptor.annotations`:

```
sh.ocx.layer.strip-components : "<u8>"        # optional; overrides Bundle.strip_components
sh.ocx.layer.prefix           : "<rel-path>"  # optional; default "" (package root)
```

Read-boundary type (parse + validate, not stored in metadata blob):

```rust
struct LayerLayout { strip: u8, prefix: RelPath }  // RelPath = validated, non-escaping
// resolve: annotation -> Bundle.strip_components -> 0
```

Precedence (per layer): descriptor annotation → `Bundle.strip_components` → `0`.
Absent annotations ⇒ identical behaviour to Part 1 ⇒ backward compatible.

### Backward-compatibility contract (LOAD-BEARING — must not regress)

`Bundle.strip_components` is **retained permanently** as the package-wide default.
Per-layer annotations are a strictly-optional override layered on top. Three
non-negotiable invariants:

1. **Read precedence is a fallback chain, never a replacement.** For each layer:
   `sh.ocx.layer.strip-components` annotation if present, **else**
   `Bundle.strip_components` (package-wide), **else** `0`. The bundle field is
   still read and honored for every layer that carries no annotation.
2. **Publish emits annotations only when explicitly requested.** The default
   publish path sets `descriptor.annotations = None` (unchanged from today). An
   `sh.ocx.layer.*` key is written **only** when the publisher explicitly
   supplies per-layer layout (e.g. `path.tar.gz:strip=1`). No implicit /
   auto-derived annotations — a package that today produces manifest M must
   produce byte-identical manifest M after this change when published the same
   way. No annotation is ever synthesized from `Bundle.strip_components`.
3. **Existing published packages are untouched.** Old manifests have no layer
   annotations, so they resolve via the bundle default exactly as before — no
   re-publish, no migration, no metadata-schema bump.

Consequence: `strip_components` on `Bundle` is **not deprecated** and remains the
recommended way to express a single strip for the whole package. Per-layer
annotations exist only for the multi-layer case where one value can't fit all
layers.

### Data Model impact

- Part 1: **none** (no type changes; `strip_components` stays on `Bundle`).
- Part 2: no metadata-schema change; annotations live on the manifest. Publish
  CLI needs a way to attach per-layer layout to each `LayerRef` (e.g.
  `path.tar.gz:strip=1,prefix=share`) — extend `LayerRef` `FromStr`
  (`publisher/layer_ref.rs:130`).

## Implementation Plan

**Part 1 (bug fix — do now):**
1. [ ] Regression test: two packages sharing one layer digest with different
       `strip_components` both assemble correct trees; assert shared
       `layers/{digest}/content/` is un-stripped (faithful).
2. [ ] Stop passing `strip_components` into the extract path — extract verbatim
       (registry `client.rs:535/610/630/646`; local `pull_local.rs:480/486`).
3. [ ] Apply package-wide strip in `assemble_from_layers`
       (`utility/fs/assemble.rs`) as a dest-path transform; skip entries that
       strip to empty (mirror current `tar.rs:185` semantics).
4. [ ] Audit other `ExtractOptions.strip_components` callers; keep behaviour for
       any non-layer extract (e.g. `package create`) unchanged.
5. [ ] `task rust:verify`, then acceptance test for reuse scenario.

**Part 2 (feature — build after Part 1 lands):**
1. [ ] `LayerLayout` type + precedence resolver; validate `prefix` (no escape).
2. [ ] Populate/read descriptor annotations (`manifest_builder.rs`, assembler).
3. [ ] Extend `LayerRef` `FromStr` + publish CLI syntax; run overlap detection
       after per-layer transform.
4. [ ] `/security-auditor` on `prefix` traversal; docs at
       `website/src/docs/reference/metadata.md` + layer/publish reference.

## Validation

- [ ] Regression test proves shared layer store is faithful under mixed strip.
- [ ] All existing tar/zip strip tests still pass (behaviour preserved, moved).
- [ ] Part 2 only: security review of `prefix` path handling; overlap errors clear.

## Links

- [subsystem-file-structure.md](../rules/subsystem-file-structure.md) — three-tier store, dedup invariant
- [subsystem-package.md](../rules/subsystem-package.md) — `Bundle` metadata
- [subsystem-oci.md](../rules/subsystem-oci.md) — manifest, descriptors, pull path
- [adr_variants.md](./adr_variants.md) — variant = tag naming (NOT layer config; clarifies scope)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-02 | Architect | Initial draft |
| 2026-07-02 | Owner | Accepted — build Part 1 + Part 2 now; Part 2 home = manifest annotations |
| 2026-07-02 | Owner | Added load-bearing backward-compat contract: `Bundle.strip_components` retained as default; annotations emitted only when explicit; byte-identical manifests for existing publish flows |
