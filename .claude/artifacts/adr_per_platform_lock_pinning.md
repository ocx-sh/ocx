# ADR: Per-Platform Leaf-Digest Pinning in `ocx.lock`

## Metadata

**Status:** Accepted
**Date:** 2026-06-13
**Deciders:** mherwig
**GitHub Issue:** [ocx-sh/ocx#142](https://github.com/ocx-sh/ocx/issues/142) (`area/package-manager`, `area/config`, `priority/critical`, `discussion-needed`)
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, no new deps)
**Domain Tags:** data, integration (lock format), package-manager, oci
**Supersedes:** Amends the `LockedTool` simplification recorded in `adr_project_toolchain_config.md:557-577` and the deferral note at `crates/ocx_lib/src/project/config.rs:37-40`.
**Superseded By:** N/A

---

## Context

`LockedTool` in `ocx.lock` pins **one** `PinnedIdentifier` per binding
(`crates/ocx_lib/src/project/lock.rs:120`):

```rust
pub struct LockedTool {
    pub name: String,
    pub group: String,
    pub pinned: PinnedIdentifier, // outer OCI image-index digest for multi-platform packages
}
```

For a multi-platform package the pinned digest is the **outer OCI image-index
digest**. Per-platform child manifests ("leaves") are picked at install/run time
by `PackageManager::resolve` → `Index::select`
(`crates/ocx_lib/src/package_manager/tasks/resolve.rs:96`, select at
`crates/ocx_lib/src/oci/index.rs:264`). This shape has two distinct problems.

### Motivation 1 — Durability / correctness (the load-bearing one)

The lock pins an object **OCX's own publish pipeline orphans by design**.
`Client::merge_platform_into_index` (`crates/ocx_lib/src/oci/client.rs:230`)
rewrites the image index on **every** platform push: fetch index at tag →
`retain(entry.platform != platform)` → add the new entry → re-serialize →
`Algorithm::Sha256.hash(index_data)` mints a **new index digest** → re-point the
tag. `push_manifest_and_merge_tags` (`client.rs:744`) repeats this once for the
primary tag (line 766) and once per cascade tag (`3.28`, `3`, `latest`) in the
loop (line 778). The OCI client has **no delete/untag capability** (grep of
`crates/ocx_lib/src/oci/` returns zero `delete`/`untag` methods), so every prior
index digest is left **untagged** in the registry — orphaned within seconds of
the next platform push, well before any registry GC runs.

A lock that pins that index digest is therefore **silently conditional on the
registry never garbage-collecting untagged manifests** — a condition the OCI
distribution spec does not guarantee (see Industry Context). Per-platform child
("leaf") manifest digests, by contrast, carry over verbatim into each rewritten
index via the `retain()` at `client.rs:291` and stay referenced by the live
tagged index; they are displaced only when the *same version + same platform* is
re-pushed with new content.

### Motivation 2 — Version divergence across platforms

A single rolling-tag lock cannot express:

1. **Vendor drops a platform mid-line.** Tool ships `linux/* + darwin/*` through
   `1.4.x`, drops `darwin/amd64` at `1.5.0`. Intel-mac CI must stay on `1.4.x`
   while linux runs `1.5.0`.
2. **Platform-specific hotfix.** A patch shipped only to `windows/amd64`; pinning
   a patch tag locks Windows while accepting unrelated drift elsewhere.

The lock-level change in this ADR covers the **durability** problem fully and the
**"vendor drops a platform"** sub-case (a dropped platform's leaf simply ceases to
be recorded on the next re-lock — see *Available-only encoding* below). The
remaining version-divergence sub-cases need a *different tag per platform* in
`ocx.toml`, which is **out of scope** here (see Decision 3 + the follow-up sketch
in Open Questions).

### Original design

`adr_project_toolchain_config.md:557-577` planned a `platforms: BTreeMap<String,
LockedPlatform>` map with `Available { manifest_digest } | Unavailable`, then
simplified to a single `PinnedIdentifier` for the first shippable increment. This
ADR re-introduces a per-platform map with the original *durability* intent intact,
but **drops the `Unavailable` variant**: the map records only platforms the
publisher ships (key present = available; key absent = not shipped). See
*Available-only encoding* in the Decision Outcome for the rationale (anti-bloat for
large platform spaces; no fragile canonical target set; matches uv/PEP-751).

---

## Decision Drivers

- **Reproducibility is the lock's reason to exist.** A pin that the publisher's
  own tooling invalidates in seconds is a broken promise. (Block-tier.)
- **No silent failures.** A dropped/unsupported platform must be an early, legible
  signal — not a late `SelectResult::NotFound` → exit 79. Under available-only this
  is satisfied without a stored marker on the two paths that matter: an absent host
  key errors at **lock-read, pre-network**, and a drop shows as a deleted key in the
  diff **when someone re-locks** (`ocx update`/`ocx lock`). Honest scope (Codex R3):
  this is **not** a *proactive* detector — `ocx lock --check` compares the lock
  against the `ocx.toml` declaration hash, **not** the live registry, so it will
  **not** flag an upstream platform drop until a re-lock happens or the affected host
  installs. A proactive "resolving drift-check" mode is a possible future addition
  (noted in Open Questions), out of scope here.
- **Anti-bloat / no fragile target set.** The lock must not scale with the universe
  of *possible* platforms, and its content must not depend on an OCX-internal
  enumeration of "all supported platforms." Recording only shipped platforms (O(shipped),
  not O(canonical)) removes both the bloat and the `canonical_set()` coupling.
- **Graceful forward-migration over a clean break.** The project default is pre-1.0
  just-break (memory: `project_breaking_compat_next_version`). For the lock file the
  user deliberately overrides this: **read both V1 and V2, write only V2**, so a
  committed V1 lock keeps working and migrates forward on the next write. Still no
  migration *prose* in user docs (`feature_no_migration_prose_in_docs`).
- **Reuse existing plumbing.** `Index::fetch_candidates` already produces exactly
  the per-platform `(digest, platform)` data the resolver needs — and exactly the
  *shipped* set (no target-set diff required).
- **Scope discipline / YAGNI.** Keep the change Small–Medium; drop the `Unavailable`
  marker (no consumer branches on it differently from plain absence); defer the
  `ocx.toml` override syntax that carries its own heavier blast radius.

---

## Industry Context & Research

**Research:** multi-agent web research + adversarial verification (workflow
`adr-per-platform-lock-142`, run `wf_b8b96d57-30a`). No standalone `research_*.md`
artifact persisted; findings inlined here.

**Trending approach — per-platform hashes in one portable lockfile is the
converged design.** Every mature *binary* lockfile now records separate content
hashes per platform artifact in a single committed file, encoding "absent on a
platform" via markers or omission:

| System | Per-platform lock mechanism | Absent-platform handling | Per-platform content hash? |
|---|---|---|---|
| **uv.lock / PEP 751 `pylock.toml`** | `[[package]]` → `wheels[]`, each wheel has its own `hashes.sha256` + a `marker` for conditional install | Implicit (no matching wheel for env tags) | **Yes** — per-wheel hash under one entry. **Closest analogue.** |
| **npm `package-lock.json` v3** | Platform variants are *separate* optional packages (`@esbuild/linux-x64`), each with own `integrity` + `os`/`cpu` arrays | Implicit (`optional: true` + host skip) | Yes, but per separate package, not a map |
| **pnpm-lock.yaml v9** | Same model as npm; all variants recorded regardless of host; exact pins for optional deps | Implicit (absent entry) | Yes, per package entry |
| **Yarn Berry + `supportedArchitectures`** | Config-driven matrix; locks all declared platforms in one file | Implicit (resolution fails) | Yes, per resolved variant |
| **Cargo.lock** | One `checksum` per crate version, platform-independent (distributes *source*) | N/A (compile-time concern) | No — single source hash |
| **Nix `flake.lock`** | Per-input `narHash` (source tree); per-system outputs live in *evaluation*, not the lock | N/A (fails at `nix build`) | No — per source input, not per binary |
| **OCX (this ADR)** | `platforms: BTreeMap<String, Digest>` in `LockedTool` (keyed by a lossless `Platform` key string); **only shipped platforms recorded** (key present = available; key absent = not shipped); **no outer index digest stored** (regenerated on every push; the update tag lives in `ocx.toml`) | **Implicit (absent key)** — exactly as uv `wheels[]` / pnpm-npm optional variants | **Yes** — per-platform leaf manifest digest |

**Key insight:** OCX's proposal mirrors uv/PEP-751 fully — per-platform leaf hashes
in one portable lockfile, **absence encoded by omission**. Every mature binary
lockfile surveyed (uv, npm v3, pnpm v9, Yarn Berry) encodes absent-platform via
omission, not a marker; an explicit `Unavailable` variant would be the one place OCX
*diverged* from the converged design, and that divergence buys no consumer-visible
capability — install error, remedy (`ocx update`), and GC are identical for "absent"
and a hypothetical "unavailable." So OCX adopts the convergent implicit-absence
convention. Cargo and Nix are not counter-evidence — neither distributes pre-built
per-platform binaries, so neither needs per-platform hashes.

**The durability risk is real, not theoretical.** The OCI Distribution Spec
(v1.0/v1.1) contains **no normative language** protecting child manifests of a
*tagged* image index from GC. Confirmed registry bugs that deleted index-referenced
children:

- `distribution/distribution` #3178 — `garbage-collect --delete-untagged` removed
  image-index children (fixed in PR #4285).
- Azure ACR CLI `purge --untagged` #131 — breaks multi-arch images (open, 2023).
- GHCR `actions/delete-package-versions` #162 — deleted children of tagged
  indexes; non-x86 arches lost, tags return `manifest unknown`.
- Harbor retention policy #22390 (2025) — deletes recently-pulled per-arch
  manifests when the parent list expires.

Spec-compliant GC (CNCF Distribution mark+sweep, Harbor's API-level referential
integrity) *does* protect index children; modern third-party cleanup tools
(`snok/container-retention-policy` ≥3.1.0, `dataaxiom/ghcr-cleanup-action`) grew
explicit multi-arch child protection. **Net:** leaf locking survives
spec-compliant untagged-manifest GC; it cannot defend against a registry operator
running non-spec-compliant child deletion (residual gap ii). OCX's own push
pipeline is a *first-order* orphaning mechanism independent of any registry GC.

---

## Considered Options

### Option 1 — Available-only per-platform leaf-digest map; outer index digest NOT stored (CHOSEN)

**Description:** Replace the single `pinned: PinnedIdentifier` on `LockedTool` with
a bare `repository: Identifier` (registry/repo, **no tag, no digest**) plus
`platforms: BTreeMap<String, Digest>` — keyed by a lossless `Platform` key string
(Display + `os_features`/`features` when present), valued
by the leaf manifest digest, **recording only platforms the publisher ships** (key
present = available; key absent = not shipped, *no `Unavailable` marker*). The outer
image-index digest is **not stored at all** — it is the object OCX orphans on every
push and provides no durable value (the update tag already lives in `ocx.toml`).
Each platform's pull identifier is reconstructed as
`repository.clone_with_digest(leaf)`; a flat `Manifest::Image` (non-index) package
records a single `"any"` entry, resolved via a host→`"any"` lookup fallback. `ocx
lock` fans out via `Index::fetch_candidates` (which returns exactly the shipped
set — no canonical target set needed); install/run/env/GC read the per-platform leaf
directly.

| Pros | Cons |
|------|------|
| Lock stores **only stable pins** (leaf digests + repo coordinates) — no fragile field that the next push invalidates | Wide, coordinated breaking change across many consumers |
| Removes the registry round-trip to re-fetch a possibly-GC'd index manifest at install | `tools_content_equal` / JSON output / schema must be redefined |
| **No bloat, no fragile target set** — records O(shipped), not O(canonical); absence encodes "not shipped"; no `canonical_set()` to maintain or reconcile against committed locks | `BTreeMap<Platform,_>` won't compile (Platform lacks `Ord`) → key by a lossless injective Platform string (needs a small Display-superset codec; dup-key guard as defensive backstop) |
| No "advisory index digest" foot-gun — nothing tempts future code to resolve from an orphan-prone digest | Requires `LockVersion` V2 bump + a retained V1 read path |
| GC reads leaf digests straight from the lock (V2); the index-blob-read path is retained only for V1 | |
| Reuses `Index::fetch_candidates`; ~1 extra round-trip per tool at lock time, none at install; **matches uv/PEP-751 implicit-absence convention** | |

### Option 2 — Additive: keep `pinned` authoritative, add `platforms` as supplementary

**Description:** Leave the index digest as the primary pin; add the platforms map
as a forward-compatible extra; install prefers the leaf, falls back to the
`Index::select` walk.

| Pros | Cons |
|------|------|
| Conceptually smaller change | `deny_unknown_fields` on `LockedTool` makes the new field a breaking parse change anyway — no compat gain |
| Install path can fall back to old behavior | **Two sources of truth** for the same intent → drift |
| | Does **not** fix durability: the old path still resolves from the orphan-prone index digest |

### Option 3 — Status quo + cascade publish ("good enough")

**Description:** Rely on the existing `--cascade` parent-tag merge
(`adr_cascade_platform_aware_push.md`) and rolling-tag locks.

| Pros | Cons |
|------|------|
| Zero lock-format change | Cascade keeps **tags** live, not historical **digests**: a lock pinned at `3.28.1` is unreproducible once `3.28.2` cascades and the `3.28.1` index is GC'd |
| | A lock pins a digest *precisely to not follow the moving tag* — cascade solves tag liveness, the opposite problem |
| | Sacrifices patch/build pinning granularity; cannot express per-platform divergence |

### Option 4 — Publisher keep-alive tag (digest-tag / immutable build tag / referrers)

**Description:** Have `ocx package push` create a canonical tag (`sha256-<hex>`,
or an immutable build tag, or a referrers entry) holding each locked index alive.

| Pros | Cons |
|------|------|
| Could keep historical indexes reachable | **Wrong side of the trust boundary**: lock robustness is a *consumer* concern, but tags require *publisher* cooperation — helps nobody locking third-party packages |
| | Mid-publish locks still orphan: the index churns per-platform leg *within one publish* before the final tag exists |
| | Per-platform tags are a docker-era anti-pattern (mutable on re-push) |

(The "unbounded tag growth" objection from the issue is real but secondary — a
single rolling keep-alive tag or the referrers API does not grow unboundedly. The
**decisive** objection is the trust boundary.) A publisher-side keep-alive flag
remains a legitimate *separate, opt-in* feature; it is not the fix here.

### Option 5 — Verify-on-install with auto-relock

**Description:** Keep the single index digest; on a 404 at install, re-resolve the
tag and rewrite the lock.

| Pros | Cons |
|------|------|
| No lock-format change | **Mutates the lock on a machine meant to reproduce it** — defeats the entire reproducibility contract a lock exists to provide |
| | Silent drift; depends on the tag still being live (it may have cascaded away) |

---

## Decision Outcome

**Chosen Option:** Option 1 — available-only per-platform leaf-digest map; the outer
index digest is **not stored in the lock at all** (bare `repository` coordinates +
per-platform leaf digests for shipped platforms only).

**Rationale:** It is the only option that fixes the durability problem at its
root (storing only the objects that survive, not the one OCX orphans), and it is the
industry-converged shape (uv/PEP-751). The "vendor dropped a platform" signal is
preserved *without* a stored marker (absent host key errors early at install; the
drop shows in the re-lock content diff). Options 3–5 each fail the core
reproducibility contract; Option 2 incurs the same breaking parse change as Option 1
with none of the durability benefit and an added source-of-truth split.

**Why the index digest is dropped, not merely demoted to "advisory."** An earlier
draft kept the index digest as an advisory field. That is self-contradictory: a
lock's whole purpose is that its pins stay valid, yet the index digest is *known*
to be invalidated by OCX's own next push, and the advisory **tag** that drives the
update mechanism already lives in `ocx.toml`. Storing a fragile, known-stale digest
in a stability artifact is a foot-gun (it tempts future code to resolve from it)
with no compensating value — `inspect`/provenance is better served by the leaf
digests + the `ocx.toml` tag. So the field is removed entirely; the lock carries
only registry/repo coordinates and the durable per-platform leaf digests.

### Platform-complete locking (design invariant)

**Every lock-mutating operation records every platform the publisher ships at that
moment — the host platform is never special at lock time.** `ocx lock`, `ocx add`,
`ocx remove`, `ocx update`, and `ocx upgrade` all route each (re-)resolved tool
through the same `resolve_one` fan-out, which walks the index children and records
**one leaf-digest entry per advertised platform** (`P_pub`). There is **no target
set to diff against**: a platform the index omits is simply *absent* from the map.
This is exactly what `Index::fetch_candidates` returns, and it is independent of the
host — so a lock committed by the first Linux developer is already complete for the
macOS and Windows CI runners (the cross-platform-reproducibility answer to npm's
pre-10.3 host-only footgun): no one is forced to re-resolve and re-commit just
because they opened it on a different OS, and Linux vs macOS resolution can never
diverge per-machine. `resolve_lock_partial` carries untouched tools forward verbatim
(complete when last locked); only re-resolved tools are re-fanned.

### Newly-available platform → explicit re-lock (no silent pickup)

A platform absent at lock time (no map key) that the publisher *later* ships
**cannot** appear under the locked pins: adding a platform changes the index, OCX
does not store the index digest, and the leaf digests already locked are unchanged.
Picking up the new platform is therefore a deliberate `ocx update <tool>` — which
re-resolves the `ocx.toml` tag, re-fans **all** platforms (adding the new platform's
key + leaf), and may move other platforms' leaves too. Symmetrically, a platform the
publisher *drops* disappears as a deleted key on the next re-lock. This is the
correct, explicit consequence of cross-platform reproducibility: platform-set changes
are opted into via an update, never silently resolved at install time.

### Install-time platform resolution (two outcomes)

The install/run/env path looks up `lock.tools[i].platforms[host]`, falling back to
`platforms["any"]` (flat `Manifest::Image` packages lock a single `"any"` entry):

| Outcome | Meaning | Behavior |
|---|---|---|
| key present (host, or `"any"`) | publisher ships this platform at the locked version | pull `repository.clone_with_digest(leaf)` directly (no `select` walk) |
| key absent (no host key **and** no `"any"`) | publisher does not ship this platform at the locked version (or the lock predates it) | clean error at **lock-read, pre-network**: "no `<host>` leaf for `<tool>` at the locked version; run `ocx update <tool>` to (re-)resolve if it has since been added" |

There is a single absent→error path — no `Unavailable`-vs-`absent` distinction. The
remedy (`ocx update`) is identical for "publisher does not ship this" and "lock
predates this platform," so the two are not separated (YAGNI: no consumer branches on
the difference). The late `SelectResult::NotFound`→exit-79 failure mode is still
eliminated because absence is decided from local lock bytes, not a live index walk.

### Honest scoping of the durability claim

The adversarial review corrected two overstatements that this ADR adopts as
binding framing:

1. **Risk reduction, not elimination.** Per-platform leaf locking does not make a
   package "GC-proof." It **shrinks** the orphaning window from *"OCX orphans the
   index on every push"* (frequent, self-inflicted) to *"a leaf is displaced only
   by a content-changing same-version+same-platform re-push, or by a registry
   operator running non-spec-compliant child deletion"* (both rare). The
   `NotFound`/exit-79 failure mode shrinks; it does not disappear.
2. **Manifest durability, not blob durability.** Leaf locking pins the *manifest*.
   An aggressive registry blob-GC can still 404 a layer blob the leaf references.
   The claim is scoped to **manifest-resolution durability**.

### Object lifetime (registry side, OCX push pipeline)

| Object | Referenced by (live) | Orphaned when | Index-digest lock | Leaf-digest lock |
|---|---|---|---|---|
| Outer image index | the tag, until next push | **every** platform push + every cascade-tag merge re-points the tag | **fragile** — OCX abandons it on the next push | **not stored** (regenerated on push; update tag lives in `ocx.toml`) |
| Per-platform leaf manifest | the **current tagged index** (carried via `retain()`, `client.rs:291`) | only when the **same version + same platform** is re-pushed with new content | n/a | **durable** across normal push/cascade churn |
| Layer blobs | the leaf's layer descriptors | when no surviving manifest references them | indirect | indirect — leaf lock pins the manifest, **not** a blob guarantee |

Carve-out: leaf survival is unconditional only for the *other* platforms. The leaf
of the platform being re-pushed with new content **is** displaced from the new
index by `retain()`.

### Residual gaps (accepted for v1)

- **(i) Same version + platform content overwrite** orphans the previously-locked
  leaf. Acceptable: versions are immutable by OCX convention; identical re-pushes
  are content-addressed (same digest, lock stays valid); only a *content-changing*
  re-push under the same version+platform breaks it — a publisher anti-pattern.
  A loud lock failure is arguably the correct tamper/reproducibility signal.
- **(ii) Non-spec-compliant registry cleanup** that deletes untagged *children* of
  a tagged index. Undefendable client-side; pre-exists this proposal. Mitigation is
  documentation-level only (recommend spec-compliant `--delete-untagged` GC; warn
  against naive child-deleting cleanup actions).
- **(iii) "never shipped" vs "dropped" is not distinguishable from a single lock.**
  Available-only encodes both as an absent key. Acceptable: no consumer branches on
  the difference (identical install error + `ocx update` remedy + GC behavior), and
  the historical answer is recoverable from the git history of `ocx.lock` (the prior
  commit's platforms map) and from the registry — not something a single lock
  snapshot needs to carry. The `Unavailable` marker encoded a fact no code path acted
  on (aligns with the project preference against over-encoding benign states).

### Consequences

**Positive:**
- The lock records only stable objects (leaf digests + repo coordinates); the
  fragile index digest is gone, so fresh-machine installs cannot depend on it.
- Install reads `lock.tools[i].platforms[host]` directly — no registry round-trip to
  re-fetch and re-resolve the index manifest; the platform-pick moves client-side.
- Platform-complete locking: a lock committed on one OS is complete for all CI
  runners; no per-OS re-resolve-and-recommit churn, no cross-machine divergence.
- Dropped/unsupported platforms surface as a clean early exit at lock-read (absent
  key), not a late `NotFound`; the drop is also visible as a deleted key in the
  re-lock content diff.
- No `canonical_set()` to define, maintain, version, or reconcile against committed
  locks; no `Unavailable` rows bloating narrowly-shipped tools (O(shipped), not
  O(canonical)).
- `clean.rs::resolve_to_package_digests` reads V2 leaf digests straight from the
  lock (no index-blob read) for V2 entries; the index-blob-read path is **retained
  only for reading V1 (legacy) entries**.
  - **Implementation note (2026-06-14, Living Design Record).** GC *rooting* reads no
    index blob for V2, but GC *reachability* does: because V2 stores only leaf digests
    (never the index digest), a held leaf would leave its parent image-index blob
    orphaned on the next `ocx clean` — a V2-introduced regression vs V1 (where the
    lock's index digest rooted the blob). The fix lives in the reachability graph (the
    single BFS authority over all three CAS tiers, per `arch-principles.md` "Ref
    separation for GC"), **not** in the lock or resolver: `reachability_graph.rs`
    synthesizes a reverse `child_leaf_blob → index_blob` retention edge by reading the
    index blobs already in the store at graph-build time and resolving each advertised
    child's shard path. The lock invariant ("index digest NOT stored") is untouched —
    nothing is read from or written to `ocx.lock`. The edge is purely *additive* (it can
    only retain, never collect): a fully-unreferenced index is still collected. This
    keeps `ocx pull` from having to re-fetch a GC'd index for a still-held leaf.
- `hook.rs` can skip its `manager.resolve()` round-trip (it has the leaf in the lock)
  and its fingerprint becomes the host leaf — more correct than the old index digest,
  which churned when *any* platform changed.

**Negative:**
- More migration code than just-break: a retained V1 read shape + the legacy
  index-digest install/GC path + the pin-preserving transcription upgrade. The
  upside (no flag-day; committed V1 locks keep working; forward-migrate on next
  write) is what the user prioritized over a clean break.
- JSON output (`ocx lock --format json`) shape changes (new `platforms`).
- External `ocx-sh/ocx-mirror` (vendors `ocx_lib`, reads `ocx.lock`) must gain V2
  read support before V2 locks reach its CI — coordination dependency. (It can read
  V1 today; the forward-incompat is mirror-reads-V2.)

**Risks:**
- *Lossy map key would regress multi-variant publishers (Block-tier if unaddressed):*
  the plain `Platform::Display` drops `os_features`/`features`, so two **advertised**
  children differing only in those would collide on the key — and because today's
  full-`Platform` `Index::select` *can* distinguish them, a lossy key would convert a
  currently-installable package into a **hard lock failure** (a capability
  regression, not just a silent overwrite). Mitigation: the key is a **lossless,
  injective** Platform encoding (no collision is structurally possible), so such a
  publisher locks correctly. The **runtime dup-key guard** in `resolve_one` +
  transcription remains as a defensive assertion (fail cleanly, never silent
  overwrite) and a structural no-collision test covers the common platforms.
- *`eq_content` / `generated_at`:* the content-equality comparator must compare the
  full platforms map (+ `repository`). `BTreeMap` equality already detects key add,
  key remove, and value change — so a platform-set change (the drop/appear signal) is
  correctly treated as content-changed and advances `generated_at`; an unchanged
  re-lock is byte-identical.
- *Host→`"any"` fallback:* the lookup must try the host key **then** `"any"` before
  declaring absent, or flat `Manifest::Image` packages regress into spurious
  absent-host errors. A correctness gate, covered by the flat-image test.

---

## Technical Details

### Data model

```rust
// crates/ocx_lib/src/project/lock.rs

#[derive(Serialize_repr, Deserialize_repr, ...)]
#[repr(u8)]
pub enum LockVersion { V1 = 1, V2 = 2 } // add V2; serde_repr rejects unknown

// V2 on-disk shape (read + write)
#[serde(deny_unknown_fields)]
pub struct LockedToolV2 {
    pub name: String,
    pub group: String,
    /// Registry/repo coordinates shared by every platform leaf — a bare
    /// Identifier with NO tag and NO digest. The per-platform pull id is
    /// reconstructed as `repository.clone_with_digest(leaf)`. The outer
    /// image-index digest is intentionally NOT stored (OCX orphans it on
    /// every push; the update tag lives in ocx.toml).
    pub repository: Identifier,
    /// Per-platform leaf digests for **shipped platforms only**. Key = a
    /// **lossless, injective** Platform string ("linux/amd64", "darwin/arm64/v8",
    /// "any"; extended to carry `os_features`/`features` when present so two
    /// distinct advertised platforms can NEVER collide on the key). A platform the
    /// publisher does not ship is encoded by ABSENCE of its key — there is no
    /// `Unavailable` marker. Flat value (just the digest) for minimal bytes;
    /// `BTreeMap<String,_>` for byte-stable output without Ord on Platform.
    pub platforms: BTreeMap<String, Digest>,
}

// V1 read-only shape (legacy locks; never written)
#[serde(deny_unknown_fields)]
pub struct LockedToolV1 { pub name: String, pub group: String, pub pinned: PinnedIdentifier }
```

On-disk (V2):

```toml
[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake"           # registry/repo coordinates; no tag, no digest

[tool.platforms]
"linux/amd64"  = "sha256:<leaf-amd64>"
"linux/arm64"  = "sha256:<leaf-arm64>"
"darwin/arm64" = "sha256:<leaf-darwin-arm64>"
# darwin/amd64, windows/amd64 simply absent = publisher ships no such leaf
```

**In-memory model + V1 read.** To read both formats while writing only V2, the
loader normalizes each on-disk tool into one in-memory enum; read paths branch, the
writer only ever serializes the V2 (`PerPlatform`) arm:

```rust
pub struct LockedTool { pub name: String, pub group: String, pub resolution: LockedResolution }

pub enum LockedResolution {
    /// Read from a V1 lock (legacy on-disk `pinned`). Only the outer index digest
    /// is known → install/run/GC use the legacy index-digest + Index::select path.
    /// Never serialized: upgraded to `PerPlatform` (pin-preserving) before any write.
    LegacyIndex(PinnedIdentifier),
    /// V2 — the only written shape. Bare repo coordinates + available-only leaf map.
    PerPlatform { repository: Identifier, platforms: BTreeMap<String, Digest> },
}
```

Serialization reuses the existing borrowed-view pattern (`SerializableView` /
`LockedToolView`): the writer projects `PerPlatform` to `LockedToolV2`; a
`LegacyIndex` reaching the writer is a bug (write commands transcribe first).

**Map-key decision (resolves the `Ord` blocker):** key by a **lossless, injective
Platform string**, **not** `BTreeMap<Platform, _>`. `Platform` derives `Hash + Eq`
but **not** `Ord` (`crates/ocx_lib/src/oci/platform.rs:50`), and its serde routes
through `native::Platform` to an OCI JSON *table* (not a string) — both make it
unusable as a TOML map key directly. Adding `Ord` would couple sort semantics into a
domain type with no natural ordering (rejected as YAGNI). `BTreeMap<String, _>` gives
byte-stable ordering for free. **The key must be injective** (the plain `Display`
form drops `os_features`/`features`): a new lossless encoding extends `Display` to
include those fields **only when present**, so the common case (`linux/amd64`) is
byte-identical to `Display` and minimal, while two children differing only in
`os_features`/`features` get distinct keys. This is what makes available-only **not
regress** the rare multi-variant publishers that today's full-`Platform`
`Index::select` can still distinguish (a lossy key would force a hard lock failure
where install succeeds today). Host lookup is a pure string compare on the same
encoding (no parse-back needed — the pull coordinate is `repository` + the digest
value). The runtime dup-key guard (see Risks) is then a **defensive assertion** that
should never fire under an injective key.

**No canonical target set, no `Platform` `JsonSchema`.** Available-only encoding has
**no** notion of "platforms the index omits," so it needs **no** `canonical_set()`
constant (the earlier draft's `P_canon`) — the resolver records exactly the children
`fetch_candidates` returns. `Platform` is only ever serialized as a lossless
*map-key string* (never as a value), so no `impl JsonSchema for Platform` is required either;
the schema describes `platforms` as a string→digest map. Both were removed from scope
by dropping `Unavailable`.

### Resolver fan-out (`crates/ocx_lib/src/project/resolve.rs`)

`resolve_one` switches from a single `index.fetch_manifest_digest(id, Resolve)`
(one index digest, `resolve.rs:378`) to `index.fetch_candidates(id, Resolve)`
(`crates/ocx_lib/src/oci/index.rs:225`), which returns `Vec<(Identifier,
Platform)>` — one entry per index child. It stores the bare `repository` (`id` with
tag and digest stripped) plus **one leaf-digest entry per advertised child**, keyed
by the child's lossless `Platform` key string. **No omitted-platform rows** are
synthesized (no `canonical_set()` diff) and **the outer index digest is discarded,
not stored.** A **runtime duplicate-key guard** fails cleanly if two advertised
children stringify to the same key (never a silent `BTreeMap` overwrite). **Cost is
~one extra round-trip per tool** (digest → full manifest parse), *not* N fetches: the
child digests + platforms are already inside the parent index manifest the single
fetch returns. Children are keyed by the **lossless injective Platform string**
(§ Map-key decision), so a multi-variant publisher whose children differ only in
`os_features`/`features` locks correctly rather than hard-failing. For a flat
`Manifest::Image` (non-index) package, `fetch_candidates`
returns a single entry with `Platform::Any` → a one-entry `platforms` map keyed
`"any"` (install resolves it via the host→`"any"` fallback). The fail-fast `JoinSet`
+ per-tool timeout + `retry_fetch` classification are preserved, and these semantics
apply identically to `resolve_lock_partial` (used by `add`/`remove`/`update` of a
subset). Offline/frozen behavior is unchanged: an unpinned-tag `Resolve` miss still
raises `PolicyResolutionBlocked` (exit 81).

### Consumer changes (authoritative blast radius)

| file:line | Today | Required change | Risk |
|---|---|---|---|
| `project/lock.rs:51-56,203` `LockVersion` / `from_str_with_path` | `LockVersion { V1 }`; full-struct deserialize | add `V2 = 2`; **peek `lock_version` first**, then deserialize the V1 or V2 per-tool shape and normalize into `LockedResolution`; an older ocx meeting a V2 lock → clean "upgrade ocx" (serde_repr already rejects unknown discriminant) | High |
| `project/lock.rs` (new) V1 read shape + `LockedResolution` enum | — | retain a V1 tool deserialize struct (`{name, group, pinned}`); add the in-memory `LockedResolution` enum; writer serializes only the `PerPlatform` arm (V2) | High |
| `cli/command/lock.rs:30-65` `Lock` flags | `--check`, `--pull/--no-pull`, `-g` | **remove** `-g`/`--group` (groups are composition-only); **no `--upgrade`** (migration is automatic); whole-file reconcile only | Medium |
| `project/lock.rs:120-131` | 3-field `LockedTool`, `deny_unknown_fields` | V2 shape `LockedToolV2 {name, group, repository: Identifier (bare), platforms: BTreeMap<String,Digest>}`; retain `LockedToolV1 {name, group, pinned}` for read | High |
| `project/lock.rs:~414` `serialize_tool_views` (ADR cited :247; Discover-verified moved) | strips advisory on `pinned` | serialize `repository` (bare) + available-only `platforms`; no advisory-strip needed | Low |
| `project/lock.rs:453-465` `tools_content_equal` | compares `name`/`group`/`pinned.eq_content` | compare `name`/`group`/`repository` + full `platforms` map; `BTreeMap` equality detects key add/remove + value change (the drop/appear signal counts as content-changed) | Medium |
| `project/resolve.rs:328-363` `resolve_one` | `fetch_manifest_digest` → single `pinned` | `fetch_candidates` → bare `repository` + one leaf per advertised child (no omitted-platform rows, **no `canonical_set()`**); runtime dup-key guard; **discard** the index digest. Plus binding-aware pin-preserving transcription to upgrade carried V1 entries (cache-first → registry, re-resolve+warn fallback) | High |
| `project/resolve.rs:175-255` `resolve_lock_partial` | carry-forward verbatim | **DELETED** — replaced by `resolve_lock_touched(candidate, pre_mutation)` (two-config signature); `add` passes the new binding as touched; `remove` passes empty touched set; untouched V1 entries transcribed by `merge_carry_forward` (exact-only); failure exit 78 when V1 index gone | Medium |
| `cli/command/pull.rs:89-98` (+ `app/project_context.rs:329` `materialize_lock`) | `t.pinned.clone()` → `pull_all(.., platforms_or_default(&[]))`; `Index::select` walk | **V2:** look up `platforms[host]`, fall back to `platforms["any"]`, reconstruct `repository.clone_with_digest(leaf)`; absent (no host key, no `"any"`) → clean error; digest-addressed pull skips the select walk. **V1 (`LegacyIndex`):** keep the index-digest + `Index::select` path (no forced upgrade on read) | High |
| `project/compose.rs:198-233` | dedup + resolve on `entry.pinned.digest()` | accept `current_platform`; **V2:** dedup + resolve on the host-platform leaf (host→`"any"` fallback); **V1:** legacy index-digest path. Signature change propagates to `command/run.rs`, `toolchain_env.rs` | Medium |
| `package_manager/tasks/hook.rs:63-95` | `AppliedEntry.manifest_digest = tool.pinned.digest()`; calls `manager.resolve()` | **V2:** host-platform leaf digest (skip the `resolve()` round-trip); **V1:** legacy path. Fingerprint changes on upgrade (acceptable; release notes) | Medium |
| `cli/command/toolchain_env.rs:294-302` `resolve_global_pinned_env` | `tool.pinned.clone().into()` | **V2:** host-platform leaf; **V1:** legacy index-digest path | Medium |
| `package_manager/tasks/clean.rs:75-104,307` `resolve_to_package_digests` / `collect_project_roots` | reads index blob, walks ImageIndex children, **presence-gated** (`path_exists_lossy`, line 135) | **V2:** read leaf digests straight from `tool.platforms`, presence-gated; **V1:** retain the index-blob-read walk. Presence-gating kept in both | High |
| `cli/command/{lock,upgrade,add,remove}.rs` LockReport `digest = t.pinned.strip_advisory()` | single digest column | emit host-platform leaf as primary digest; full platforms map in verbose/JSON | Low–Medium |
| `cli/command/upgrade.rs:239-248` `lock_content_matches` | `x.pinned.eq_content(y.pinned)` | compare `repository` + platforms maps (`upgrade --check` must see platform diffs) | Medium |
| `cli/api/data/lock.rs:1-52` `LockEntry`/`LockReport` | `{binding, group, digest}` | add `platforms` map; **JSON API breaking change** | Medium |
| `oci/platform.rs:50,207` | no `JsonSchema`, no `Ord`; only host-relative `supported_set()`; `Display` lossy | **Modify (small):** add a **lossless injective `Platform`→key-string** encoding (Display + `os_features`/`features` when present) used for V2 map keys + host lookup. Still **no** `canonical_set()`, **no** `impl JsonSchema for Platform`, **no** `Ord`. The dup-key guard in `resolve_one` becomes a defensive assertion atop the injective key | Low |
| `package_inspect.rs` (ChainRole::Index) | resolves a **live** identifier, builds its own chain | **no change** — not a lock reader; `ocx package inspect` operates on a live `--platform`-resolved identifier, not `ocx.lock` | None |
| `project/lock.rs` `platforms` value type | — | value is `Digest` (already has serde + a manual `JsonSchema` impl) — **no new `LockedPlatform` type** (the enum is gone with `Unavailable`) | None |
| `ocx_schema/src/lib.rs:42` + `tests/schema_outputs.rs:97,128-192` | schema `$id` = `…/project-lock/v1.json`; pins `lock_version` enum to `[1]` | **Decide the two version axes** (see Open Questions); update pin to `[2]`; add `platforms` schema assertions | Medium |

**Acceptance tests (Python) — structural + behavioral consumers:**

| file:line | Why it breaks / matters |
|---|---|
| `test/tests/test_lock.py:141,155-214,268-344` | `_pinned_values` regex + 3-field `[[tool]]` shape assertions |
| `test/tests/test_upgrade.py:70-84` | **own** `_PINNED_RE` field-adjacency regex — High structural break (not just semantic) |
| `test/tests/test_pinned_offline.py:77-79` | reads `pinned`, exec offline after wiping the tag store — this **is** the orphan/404 scenario the ADR fixes; must read the per-platform leaf instead |
| `test/tests/test_oci_registry_mirror.py:46-48,344-354` | **own** `_PINNED_RE`; asserts the lock records the canonical (not mirror) host+digest |
| `test/tests/test_toolchain_env.py:592-607` | exercises the **global** `$OCX_HOME/ocx.lock` path; lenient corrupt-lock contract |
| `test/tests/test_project_add.py:255`, `test_project_lifecycle.py:103` | substring asserts on `ocx.lock` text (lower risk — survive format change unless the per-repo line form changes) |
| `test/tests/test_schema_generation.py:61,206-247` | asserts schema `$id` v1.json + `lock_version` enum `[1]` |
| `test/tests/test_clean_project_backlinks.py`, `test_project_pull.py` | exercise GC rooting + project install through the new path |

**Documentation surfaces** (per `feedback_plans_must_list_docs`): the `ocx.lock`
shape / "locking" narrative appears in `website/src/docs/in-depth/project.md:37-54`
(primary — replace "image-index manifest digest" with per-platform leaf wording +
V2 TOML example), `user-guide.md:416-419`, `faq.md:45`,
`reference/configuration.md`, `reference/command-line.md:340-394`,
`reference/environment.md`, `reference/env-composition.md`, `getting-started.md`.
No migration prose (pre-1.0 policy) — state the regenerate-by-re-run consequence
plainly, nothing about "former form removed."

### Lock format migration (V1 → V2): read both, write only V2

The migration is a **forward-migration** (read N−1 and N, write N), not a flag-day.
This is a new pattern for OCX — `LockVersion` and `declaration_hash_version`
currently only *reject* unknown versions; there is no migration precedent, so this
ADR establishes it.

- **Read both.** This ocx reads **V1 and V2** locks. The loader peeks
  `metadata.lock_version` first (lenient header parse) and deserializes the matching
  per-tool shape — V1 (`{name, group, pinned}`) or V2 (`{name, group, repository,
  platforms}`). `#[serde(deny_unknown_fields)]` stays on each shape; the version peek
  selects which applies, so a V1 lock no longer trips a raw `TomlParse`. An **older**
  ocx reading a **V2** lock sees an unknown `lock_version` (serde_repr rejects) →
  clean *"lock written by a newer ocx; upgrade ocx"*. That forward-incompat is
  unavoidable and correct.
- **Write only V2.** No code path emits V1. Every lock-mutating command (`ocx lock`,
  `add`, `remove`, `update`, `upgrade`) writes V2 — so a V1 lock migrates forward the
  moment anyone changes it. "Want to change your lock ⇒ you upgrade" is enforced by
  construction: there is no V1 writer.
- **In-memory normalization.** The loader normalizes both on-disk shapes into one
  `LockedResolution` enum (see Data model). Read-path consumers branch on it: a V1
  entry uses the legacy index-digest + `Index::select` path; a V2 entry reads the
  per-platform leaf directly. A committed V1 lock therefore keeps installing/running
  **offline, with no forced upgrade and no read-path network or mutation** — it works
  as well as it did before (still carrying the V1 durability fragility, which the
  first write fixes).

**Upgrade fidelity (when a write turns a V1 entry into V2) — pin-preserving transcription, fail-closed:**

The decisive insight is that V1→V2 migration is **not** re-resolution. A V1 entry stores
`registry/repo@sha256:<index-digest>`. Fetching its children (`fetch_candidates` on the
digest-addressed index) yields the **exact same leaves** every time the index manifest is
retrievable — deterministic and pin-preserving. Only live-tag re-resolution (looking up
where `:tag` points *today*) drifts. This distinction drives every decision below.

- `ocx lock` — whole-file reconcile: re-resolves every tag from `ocx.toml`, emits V2. A
  clean run (declaration hash matches) is byte-identical (idempotent no-op). A dirty run
  (declaration hash drifted) re-resolves all declared tags — a moving tag may advance.
- `ocx upgrade` — whole-file forced bump: always re-resolves every declared tag against
  the live registry, emits V2. The only place live-tag re-resolution is intentional.
- `add`/`remove` — partial mutators: carry every **untouched** V1 entry forward verbatim
  by reading its pinned index digest and transcribing the per-platform leaves
  (exact-only, `transcribe_v1_to_v2(exact_only=true)`). This migration is automatic and
  pin-preserving — no explicit "migrate" verb needed. If the V1 index digest is no longer
  retrievable, the command **fails with exit 78** directing the user to run `ocx upgrade`
  (Codex-R2: an unrelated `add toolX` must never drift `toolY` onto a different platform
  set). The re-resolve fallback that `--upgrade` would have provided is deleted; there is
  no silent re-resolution from mutators.
- **`ocx lock --upgrade` is removed.** Its former job — "migrate the format without
  moving pins" — is now an automatic side effect of any write. The flag is deleted;
  migration is handled by the carry-forward transcription path on every `add`/`remove`.
- **Groups are not a lock or upgrade scope.** Groups scope which tools `ocx run`,
  `ocx env`, and `ocx pull` see. The `--group`/`-g` flag and positional PKGS scoping
  are removed from `lock` and `upgrade`.
- **Codex-R2 (anti-drift) and Codex-H1 (anti-laundering)** are upheld **structurally**:
  the offending path — a `resolve_lock(&[])` full live re-resolve reachable from
  `add`/`remove` — is deleted, not guarded. There is no `ensure_untouched_v1_transcribable`
  guard to delete; the structural impossibility is the guarantee.

**Caveat (honest):** the pin-preserving carry-forward depends on the V1 index digest
still being fetchable (cached locally, or not yet registry-GC'd). On the machine that
wrote the V1 lock (or soon after) it is cached → offline + exact. On a fresh clone
where the index is already gone, `add`/`remove` fail with exit 78 — the user must run
`ocx upgrade` to advance to live-tag resolution. A plain `ocx lock` also works (it
re-resolves all tags anyway).

`declaration_hash` (over `ocx.toml` content) is **unchanged** — the platforms map is
a resolver-side artifact and must **not** enter the hash input (a per-tool tag
override *would* change the hash → the deferred follow-up's
`DECLARATION_HASH_VERSION` bump, not this ADR's).

---

## Decisions Resolved (from issue #142)

1. **Is demand strong enough to revisit the simplification?** **Yes.** The label is
   correctly `priority/critical`: the lock pins an identity OCX's own publish
   pipeline invalidates by design, and confirmed registry GC bugs delete
   index-referenced children. This is a durability/correctness fix, not just
   flexibility.
2. **V2 migration vs "just break"?** **V2 with a forward-migration, not just-break.**
   Bump `LockVersion` to **V2**; **read both V1 and V2, write only V2.** A committed
   V1 lock keeps working on read paths (legacy index-digest path, offline, no forced
   upgrade); it migrates forward the instant any command rewrites it, because no
   writer emits V1. The upgrade is **pin-preserving** (transcribe the V1 index's
   children into per-platform leaves, cache-first → registry) with a re-resolve
   fallback (+warn) when the V1 index is gone. New `ocx lock --upgrade` does the
   format migration without re-resolving live tags. (This overrides the project's
   default "pre-1.0 just-break" policy for this case, by explicit decision — the
   graceful read-old/write-new path is worth the extra migration code.)
3. **`ocx.toml` per-platform override syntax — co-design or defer?** **Defer to a
   follow-up issue.** The seam is verified in code: `ocx.toml` tool tables are
   `BTreeMap<String, Identifier>` (one tag per binding), and `ocx lock` already has
   everything (`fetch_candidates`) to fan a single tag into a full per-platform map
   with **zero `ocx.toml` change**. The override adds a strictly different
   capability (different *tags* per platform) that forces a heavier blast radius
   (`DECLARATION_HASH_VERSION` bump + `StaleLockOnPartial` re-reasoning). Deferring
   removes **zero** downstream work from this change (install/compose/GC consume the
   lock, not `ocx.toml`). File the follow-up now so the divergence-hotfix motivation
   is captured.

---

## Implementation Plan

1. [ ] **Schema + versioned read.** `LockVersion::V2`; V2 on-disk `LockedToolV2`
   (`repository` + available-only `platforms: BTreeMap<String,Digest>`); retain
   `LockedToolV1` (`pinned`); in-memory `LockedResolution` enum; version-peek loader
   (read both, normalize); apply the schema `$id` axis → `v2.json` (Open Q1). **No**
   `LockedPlatform` type, **no** `JsonSchema` for `Platform` (Platform is only a
   lossless map-key string; add that small Display-superset codec). Round-trip +
   determinism + read-V1 + read-V2 tests.
2. [ ] **Resolver fan-out + transcription.** `resolve_one` via `fetch_candidates` →
   bare `repository` + one leaf per advertised child (no omitted rows, **no
   `canonical_set()`**); **runtime duplicate-Display-key guard** (fail on collision);
   index digest discarded; binding-aware pin-preserving transcription helper (V1
   `pinned` → V2 via `fetch_candidates`, cache-first → registry, re-resolve+warn
   fallback, **offline/frozen ⇒ `PolicyResolutionBlocked` on uncached**); extend
   `tools_content_equal` / `lock_content_matches` to `repository` + the platforms map.
3. [ ] **Write-only-V2 (automatic migration).** `add`/`remove` carry untouched V1 entries
   forward via `merge_carry_forward` (transcription, exact-only, exit 78 on miss);
   `lock`/`upgrade` write V2 unconditionally via `resolve_lock(&[])`. No `--upgrade` flag.
   Delete `resolve_lock_partial`, `ensure_untouched_v1_transcribable`, `touched_bindings`,
   `upgrade_lock`, the `-g`/PKGS flags from `lock`/`upgrade`, and the partial branch from
   `upgrade`. Assert no path serializes a `LegacyIndex`.
4. [ ] **Read paths (install / compose / hook / global-env).** V2: look up
   `platforms[host]` → fall back to `platforms["any"]` → reconstruct
   `repository.clone_with_digest(leaf)`; two outcomes (present → pull; absent → clean
   pre-network error); `hook.rs` drops `resolve()`, fingerprints the resolved leaf.
   V1: keep the legacy index-digest + `Index::select` path (no forced upgrade on
   read). Thread `current_platform` into `compose_tool_set` (shared host→`"any"`
   lookup helper).
5. [ ] **GC.** V2: root leaf digests from the lock, **presence-gated**. V1: retain
   the index-blob-read walk in `resolve_to_package_digests` (presence-gated).
6. [ ] **Output + structural guards.** `LockEntry`/LockReport platforms field;
   structural test "no V2 lock stores an index digest"; structural test "Display
   keys do not collide for the common platforms"; runtime dup-key guard test.
7. [ ] **Tests + docs.** Update the Rust + Python acceptance consumers above and
   the doc surfaces; regressions for (a) offline-after-tag-wipe resolving the leaf
   where the index would 404, (b) read-a-V1-lock-then-install still works, (c)
   `add`/`remove` V1 carry-forward: transcription succeeds when cached, exit 78 when
   index gone, exit 65 when declaration hash drifted, (d) re-lock platform appears/
   disappears, (e) flat `Manifest::Image` `"any"` installs on every host,
   (f) `ocx upgrade` and `ocx lock` are whole-file (no `-g`/PKGS).
8. [ ] **Coordinate `ocx-sh/ocx-mirror`** gains V2 read support before V2 locks reach it.

Each step is contract-first TDD with its own review-fix loop (route via
`/swarm-execute`).

## Validation

- [ ] `ocx lock` records one leaf per **shipped** platform; platforms the index
  omits are simply absent from the map (no `Unavailable` rows, no `canonical_set`).
- [ ] A committed **V1** lock still installs/runs/`env`s **offline** (legacy
  index-digest path) with no forced upgrade and no read-path mutation.
- [ ] Any write command (`add`/`remove`/`update`/`lock`) rewrites a V1 lock as V2;
  no path emits V1.
- [ ] `ocx lock --upgrade` does **not exist** — migration is automatic on any write; the validation row is removed.
- [ ] `add`/`remove` carry untouched V1 entries forward by reading their pinned index digest (transcription, not re-resolution); if the V1 index is unretrievable, the command fails exit 78 directing the user to `ocx upgrade`.
- [ ] `add`/`remove` check the pre-mutation declaration hash before carry-forward; a drift (hash mismatch) fails exit 65 before touching anything.
- [ ] `ocx upgrade` re-resolves every declared tag (whole-file forced bump); no `-g`/PKGS subset; exit 81 when policy blocks re-resolution.
- [ ] An older ocx reading a V2 lock fails with a clean "upgrade ocx" message.
- [ ] `ocx.lock` is byte-identical across Linux/macOS/Windows CI runners; an
  unchanged re-lock is byte-identical (no `generated_at` churn).
- [ ] Install on a fresh machine resolves from the leaf with the orphaned index
  digest GC'd (offline-after-tag-wipe regression passes where it 404s today).
- [ ] `ocx clean` roots only on-disk leaves (single-platform machine accumulates no
  phantom roots).
- [ ] Platform-complete locking: a lock written on Linux records every shipped
  platform; a macOS runner reads it without re-resolving or re-committing.
- [ ] **Re-lock transitions:** a newly-shipped platform appears as an added key on
  `ocx update`; a dropped platform disappears as a removed key (both flagged
  content-changed); install on a host whose key is absent gives a clean pre-network
  error naming `ocx update`.
- [ ] Flat `Manifest::Image` package locks a single `"any"` entry and installs on
  every supported host via the host→`"any"` fallback.
- [ ] No **V2** lock contains an index digest (structural test on the serialized shape).
- [ ] env composition (`compose`) resolves the same host-leaf digest install pulls.

## Open Questions

1. **Schema `$id` version axis.** The schema URL is hard-coded to
   `…/project-lock/v1.json` (`ocx_schema/src/lib.rs:42`), a separate axis from the
   on-disk `lock_version` field. **Recommendation:** advance the `$id` to
   `…/project-lock/v2.json` in lock-step with `LockVersion::V2` so the two axes stay
   aligned and validators do not check V2 documents against a v1 schema.
2. ~~**Canonical target-platform set sourcing.**~~ **Resolved / removed by
   available-only.** `Unavailable` is dropped, so there is no omitted-platform set to
   compute — `Platform::canonical_set()` is **not** added, and `supported_set()`
   stays the host-relative install filter. (The outer index `pinned` is also dropped,
   not kept advisory — see Decision Outcome.)
3. **Reserve design space for the deferred override.** A future per-tool tag override
   that resolves the *same* platform to different leaves across tag selections would
   need the `platforms` value to carry the resolving tag/identifier — i.e. widen the
   flat `Digest` value back to a small struct. Pre-1.0 this is acceptable as **its own
   lock-format bump** in that follow-up (we break format freely pre-1.0); the flat
   value is chosen now for minimal bytes per the anti-bloat driver. Note the
   reservation; do not pre-build the struct (YAGNI).
4. **Proactive platform-drift detection (future, optional).** Available-only surfaces
   an upstream platform drop only on re-lock (key vanishes) or at install on the
   affected host (absent-key error) — `ocx lock --check` (declaration-hash) does not
   re-query the registry, so it will not proactively flag it. If CI ever needs
   proactive detection, a future *resolving* drift-check mode (re-resolve tags and
   diff the resulting platform set against the lock) could be added. Out of scope
   here; noted so the limitation is explicit, not implied-away.

**Title:** `feat(project): per-platform tag overrides in ocx.toml`
**Depends on:** #142.
**Gap:** the lock map fans a *single* tag per platform; it cannot pin a *different
tag* per platform (platform-specific hotfix; staggered vendor rollout). Today the
only escape hatch is splitting the tool across `[group.*]` — wrong abstraction.
**Sketch:** `[tools.cmake] default = "ocx.sh/cmake:3.28.1"` +
`"darwin/arm64" = "ocx.sh/cmake:3.28.0"` (platform keys reuse this ADR's lossless
Platform key string). **Implications:** `ProjectConfig` tool value widens from
`Identifier` to `Scalar(Identifier) | PerPlatform { default, overrides }`; multiple
identifier strings per binding enter the declaration hash →
`DECLARATION_HASH_VERSION` 1→2 + `StaleLockOnPartial` re-reasoning.
**Open questions:** same-repo-only override? Silent absence vs hard error when an
override names an unshipped platform? group-level table support? direct-digest
override? `ocx lock --update <tool>` re-resolves all platforms or only `default`?

## Links

- Issue: https://github.com/ocx-sh/ocx/issues/142
- `adr_project_toolchain_config.md` (original `LockedTool` design, §557-577)
- `adr_cascade_platform_aware_push.md` (parent-tag platform merge — the orphaning mechanism)
- `adr_index_routing_semantics.md` (`IndexOperation::Resolve`, offline/frozen policy)
- `adr_project_gc_symlink_ledger.md` (project GC roots)
- Workflow run: `wf_b8b96d57-30a` (`adr-per-platform-lock-142`) — 6 analyses + 2 adversarial verifications
