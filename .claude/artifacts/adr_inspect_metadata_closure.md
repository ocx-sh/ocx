# ADR: Metadata-Only Dependency Closure for `ocx package inspect`

- **Status:** Proposed (2026-07-20)
- **Deciders:** owner (via team-lead), architect worker
- **Domain Tags:** api · packaging · oci · cli
- **Tech Strategy Alignment:** Follows Golden Path (Rust 2024 / Tokio). No deviation.
- **Related:**
  - `adr_declared_binaries_metadata.md` (the `Bundle.binaries` claim + env-report attribution this walk aggregates; §4 Decision A admission model this mirrors metadata-side)
  - `adr_two_env_composition.md` (`through_edge`/`merge` visibility algebra reused verbatim)
  - `adr_dependency_manifest_pinning.md` (deps pinned to platform-manifest digests at authoring → the walk is pure digest-addressed)
  - `adr_index_routing_semantics.md` (`IndexOperation::{Query,Resolve}`; digest-addressed reads are local-first every `ChainMode`)
  - `adr_three_tier_cas_storage.md` (blob CAS = the persistence tier goals 4+5 already ride)
  - `research_metadata_closure_patterns.md`, `research_oci_metadata_cache_tech.md`, `research_oci_digest_cache_domain.md`
  - `subsystem-package-manager.md`, `subsystem-cli.md`, `subsystem-cli-api.md`, `subsystem-oci.md`

---

## Context / Problem

`ocx package inspect` is already a read-only, metadata-only view of what sits at a
reference (candidates / metadata+layers / resolution chain — `tasks/inspect.rs`).
It answers "what is *this* manifest," but not "what is the whole dependency
**closure**, and what executables would land on the interface `PATH` if I
installed it." Today that question is only answerable **after installing**: `ocx
package deps` reads the installed `resolve.json` transitive closure (TC), and `ocx
env`'s `binaries`/`entrypoints` arrays (`adr_declared_binaries_metadata.md` §4)
project the *installed* composed set.

The owner's intent (verbatim):

1. `package inspect` supports `binaries` incl. **transitive dependencies**.
2. Dependencies state their **linkage visibility**.
3. Interface sufficient to enumerate **ALL interface binaries WITHOUT
   loading/installing** a package.
4. Loaded manifests/configs **may persist** into the local blob cache.
5. Local blob storage is **always queried first** for digest-specific lookups.

Goals 4 and 5 are **already implemented** and must be *pinned*, not re-plumbed:
`ChainedIndex::fetch_blob` is local-CAS-first with write-through via
`stage_blob_bytes` (`chained_index.rs:479-506`); digest-addressed `fetch_manifest`
is local-first in **every** `ChainMode`; `load_config_metadata`
(`tasks/common.rs:137-159`) fetches the config blob through that same offline-aware
`Index::fetch_blob`. The only missing piece is a **metadata-only closure walker**:
there is none today — the install-time `ResolvedPackage::with_dependencies`
requires an already-installed `resolve.json`.

Because every `Dependency.identifier` is a `PinnedIdentifier` with a **required
digest** (`dependency.rs:121-140`), and `ocx package create` pins each dep to a
**platform-specific manifest digest** (`dependency_pinning.rs` — never an index
digest, GC hazard), the closure walk is **pure digest-addressed**: cycles are
cryptographically impossible (a child cannot name a digest that hashes over
itself), and diamonds are real but dedup by content identity.

## Decision Drivers

- **Offline-first (Principle #2) / backend-first (Principle #1):** the answer must
  come from cache-first metadata reads and be a first-class JSON contract.
- **Extend, don't duplicate:** `inspect` already owns the metadata-tier read path
  and the `binaries` three-way render; the closure rides that, not a new pipeline.
- **Fail-closed honesty:** a partial closure must never render as a smaller
  *complete* one ("couldn't determine ≠ determined zero" — the declared-binaries
  review doctrine).
- **Reuse the visibility algebra:** `through_edge`/`merge` already model exactly
  the interface-propagation this needs; the walk is a metadata-tier mirror of
  `with_dependencies`.

## Industry Context & Research

- **`research_metadata_closure_patterns.md`** (High confidence): every mature PM
  splits *metadata resolution* (cheap, cacheable, network-optional) from *content
  fetch*. Present the machine output as a **flat digest-keyed node array** (cargo
  `resolve.nodes` shape) — diamond-/cycle-safe by construction; a naive nested
  JSON tree cannot represent a diamond without duplication. Human output = a
  `cargo tree`-style tree with a `(*)` repeat-visit marker keyed by **content
  digest**. YAGNI on `--no-dedupe`/`--depth`/`-e` edge-filter flags until a second
  real use case.
- **`research_oci_metadata_cache_tech.md`**: OCX's `LocalIndex`+`ChainedIndex`+
  `ChainMode`+`IndexOperation` design is already a *superset* of ORAS/crane/
  regclient/containerd. **No architecture change.** Build the walker on the
  existing `Index` facade.
- **`research_oci_digest_cache_domain.md`**: digest content is immutable → serving
  it from cache is spec-conformant forever; verify-on-write already conforms.
  Concrete gap flagged: **no client-side size cap on manifest/config blob reads
  before parse** (CWE-400 sibling of the layer caps) — the walk amplifies this
  (N untrusted config blobs, not one).

---

## Considered Options

### Decision D1 — How the closure is requested (CLI surface)

**Option 1 (chosen): new `--deps` flag on `ocx package inspect`.**

| Pros | Cons |
|------|------|
| Lands where the metadata-tier read path already lives (`inspect_all` → `load_config_metadata`, cache warming already sanctioned) | Name shares a token with the `deps` **command** (mitigated: see D4) |
| Additive to the existing candidates/manifest/resolved trichotomy; existing JSON bodies unchanged | `--deps` on an image-index root must platform-select to read root metadata (documented behavior) |
| One flag yields both the closure and the interface-surface aggregate | |

**Option 2 (rejected): extend `ocx package deps` with a metadata/`--uninstalled` mode.**
`deps` is *definitionally* installed-tier — it calls `find_all` and reads
`resolve.json`. The whole point of this feature is "**without** installing," so
`deps` cannot answer it without becoming a second, contract-confusing pipeline
inside one command (installed TC vs metadata closure). Rejected: violates the firm
toolchain/OCI-tier and installed/metadata-tier layer-purity rule.

**Option 3 (rejected): new subcommand `ocx package closure`.**
A whole new command + `api/data` type + docs surface for what is a *mode* of the
existing metadata read. Rejected on KISS/YAGNI — `inspect` already resolves the
root, loads its metadata, and renders trees; the closure is one more section.

**Chosen: Option 1.** `--deps` is a presence flag (like `--resolve`), not a
paired toggle — it adds a mode, it is not a boolean-with-default needing
`--no-deps`, so the paired-toggle convention (`options::Pull`/`BinScan`) does not
apply. Flags precede the positional `PACKAGE...` (owner convention).

**Interaction with the existing trichotomy** (the genuinely contested part):

- Single image **manifest** root (flat tag / `@digest`): metadata is already in
  the `Manifest` body; `--deps` attaches the closure. `-p` not consulted (matches
  current Manifest mode).
- Image **index** root: `--deps` **platform-selects the root** (honoring `-p`,
  host default otherwise) because a closure walk needs a concrete root manifest to
  read declared deps from. This is the same selection `--resolve` performs, so
  `--deps` on an index root emits the **Resolved** body (the resolution genuinely
  happened) plus the closure. `--resolve` is therefore *redundant-but-accepted*
  with `--deps` on an index root. `--deps` alone on an index root is **never** the
  metadata-less `Candidates` body — it must select to answer.
- `-p/--platform` applies with `--deps` exactly as it does with `--resolve`.

Mental model, one sentence in the help: *"`--deps` shows the dependency closure;
for a multi-platform reference it first selects a platform (honoring `-p`) to read
the root's declared dependencies."*

### Decision D2 — Output contract

**Chosen: flat digest-keyed node array for JSON + an aggregated interface-surface
object; a `(*)`-deduped tree for plain.** (Per `research_metadata_closure_patterns.md`.)

**Option A (chosen): flat `closure` node array + `interface_surface` object.**

**Option B (rejected): nested recursive JSON tree** — cannot represent a diamond
without duplicating a whole subtree; every peer (npm ls / pnpm list --json) treats
this as a known weakness, not a pattern to copy.

**Option C (rejected): `ldd`-style flat name list** — throws away the edge
structure and per-node visibility that goal #2 explicitly requires.

JSON is **additive** to the existing inspect bodies. When `--deps` is set, the
report gains two sibling keys alongside the existing `metadata`/`layers`(/
`resolution`).

> **Contract invariant for script authors (panel S2):** `--deps` on an
> image-index root ALWAYS yields the `Resolved` body (with `resolution` +
> `platform`); on a single-manifest root it yields the `Manifest` body. A
> consumer must branch on `--deps`-mode by reading `closure` presence, never by
> `resolution` presence (which encodes root *kind*). Stated verbatim in the CLI
> help and command-line.md.

```jsonc
{
  "identifier": "ocx.sh/toolchain:1.0",
  "pinned_digest": "sha256:...",
  "platform": "linux/amd64",         // present only when the root was an index (Resolved body)
  "metadata": { /* unchanged root metadata */ },
  "layers":   [ /* unchanged */ ],
  "resolution": { /* unchanged, present only under Resolved */ },

  "closure": [                        // flat, digest-keyed, deps-before-dependents, root last
    {
      "identifier": "ocx.sh/zlib@sha256:bbbb",
      "digest": "sha256:bbbb",
      "effective_visibility": "public",     // composed from the root via through_edge/merge
      "binaries": ["z-tool"],               // tri-state: key ABSENT = undeclared; [] = asserted-zero
      "entrypoints": ["zfmt"],
      "dependencies": []                    // declared edges of THIS node (see below)
    },
    {
      "identifier": "ocx.sh/cmake@sha256:cccc",
      "digest": "sha256:cccc",
      "effective_visibility": "sealed",
      "entrypoints": [],                    // no "binaries" key → cmake declares none (undeclared)
      "dependencies": [
        { "identifier": "ocx.sh/zlib@sha256:bbbb", "visibility": "public", "name": "zlib" }
      ]
    },
    {
      "identifier": "ocx.sh/toolchain@sha256:aaaa",
      "digest": "sha256:aaaa",
      // no "effective_visibility" — the composed-from-root axis is undefined for
      // the root itself; the key is ABSENT iff "root": true (panel W3)
      "root": true,
      "binaries": [],
      "entrypoints": ["cc"],
      "dependencies": [
        { "identifier": "ocx.sh/cmake@sha256:cccc", "visibility": "sealed", "name": "cmake" },
        { "identifier": "ocx.sh/zlib@sha256:bbbb",  "visibility": "public", "name": "zlib" }
      ]
    }
  ],

  "interface_surface": {              // the "sufficient without installing" answer (goal #3)
    "binaries":    [ { "name": "z-tool", "package": "ocx.sh/zlib@sha256:bbbb" } ],
    "entrypoints": [ { "name": "cc",     "package": "ocx.sh/toolchain@sha256:aaaa" },
                     { "name": "zfmt",   "package": "ocx.sh/zlib@sha256:bbbb" } ],
    "binaries_complete": true,        // false when any interface node has UNDECLARED binaries
    "conflicts": {                    // Codex C2: unrealizability made machine-readable; always present
      "entrypoints":  [],             // [{ "name": "fmt", "packages": ["...@sha256:x", "...@sha256:y"] }]
      "repositories": []              // [{ "repository": "ocx.sh/zlib", "digests": ["sha256:x", "sha256:y"] }]
    }
  }
}
```

**Encoding decisions:**

- **`binaries` tri-state on the wire** mirrors `Bundle.binaries` exactly
  (`#[serde(skip_serializing_if = "Option::is_none")]`): key **absent** =
  undeclared; `[]` = publisher asserts zero interface executables; `[names]` =
  declared. This distinction is load-bearing for `binaries_complete` (below).
- **`effective_visibility`** is the composed-from-root visibility
  (`through_edge`+`merge`), one of the four wire strings. It is the "linkage
  visibility" of goal #2 as *seen from the root*. Each node's `dependencies[]`
  additionally carries the **declared** edge `visibility` (as authored), so both
  the declared edge and the effective composition are visible.
- **`root: true`** flags the inspected package itself (serialized only when true).
  The root carries **no `effective_visibility` key** — that axis means "visibility
  as composed from the root" and is undefined for the root itself; overloading the
  authored wire value `public` as a sentinel would let consumers filtering
  `effective_visibility == "public"` wrongly include the root (panel W3). Rule:
  the key is absent **iff** `root: true`. The root is admitted to the interface
  aggregate unconditionally regardless.
- **`interface_surface`** is the aggregated answer to goal #3, admission rule
  identical to the composer (`adr_declared_binaries_metadata.md` §4 Decision A):
  the **root unconditionally**, each **dep iff `effective_visibility.has_interface()`**.
  `binaries`/`entrypoints` reuse the `{ name, package }` shape of
  `api::data::env::BinaryAttribution` (its `from_pairs<T: Display>` is already
  generic and reused verbatim). Both arrays are always present (possibly empty).
- **`binaries_complete`** is the explicit "unknown vs zero" ruling (see Edge
  Cases): `false` when **any** interface-admitted node has *undeclared* binaries
  (`None`). Entrypoints have no such flag — the entrypoint **map keys are
  authoritative**, so the entrypoint aggregate is always complete.
- **`conflicts`** (Codex C2) closes the "authoritative-looking but unrealizable"
  gap: install/compose HARD-reject (a) duplicate interface entrypoint names
  (`composer::check_entrypoints` via `pull.rs`) and (b) one repository at two
  visible digests (`check_repo_digest_conflicts`). Inspect stays a **view, not a
  gate** — exit 0, same precedent as the `deps` command's non-fatal
  `warn_repo_digest_conflicts` (the fatal/diagnostic split is standing design) —
  but both conditions are detected over the interface projection as **pure
  post-processing on already-gathered metadata** (zero extra I/O) and reported
  machine-readably. Both arrays always present (empty = the surface is
  realizable). A consumer needing an install-equivalent answer checks
  `conflicts.*` emptiness; the plain render notes each conflict as a leaf.

**Plain format** (inspect holds the single-table tree exemption):

- A `closure` branch rendered as a **tree** rooted at the inspected package,
  following declared edges, with a `(*)` marker on any node visited a second time
  (dedup keyed by content digest — a diamond's shared node renders in full once,
  `(*)` on repeat). Each node annotates its `effective_visibility` (reusing the
  existing `SemanticAnnotation::Visibility` palette) and lists its
  binaries/entrypoints as leaves (three-way binaries render reused from
  `binaries_node`).
- An `interface surface` branch summarizing the aggregate — binaries + entrypoints
  leaves, and when `binaries_complete == false` a note leaf, e.g.
  `binaries (incomplete: N interface deps declare no binaries)`.

Plain = the `cargo tree` register; JSON = the `cargo metadata` register. The flat
JSON array is the primary machine surface; the tree is the human glance.

### Decision D3 — Walker contract

**Chosen: a two-phase, module-private free-function walker in `tasks/inspect.rs`,
reusing `fetch_manifest(Op::Resolve)` + `load_config_metadata` + the
`through_edge`/`merge` algebra; fail-closed per node.**

The walker is **not** a new `pub` facade method — per the package-manager module
architecture, only the existing `inspect`/`inspect_all` facade methods are `pub`;
the walk is module-private free functions taking explicit params. It is folded
into the existing `inspect` flow (which already resolves the root and loads its
metadata) so the root's `ValidMetadata` is loaded **once** and handed to the walk.

**Two phases** (mirrors the composer's parallel-preload-then-sequential-emit):

- **Phase 1 — parallel metadata gather (I/O bound).** BFS the DAG from the root's
  declared deps, dedup the frontier by advisory-stripped digest identity, and
  fetch each **unique** node's `(ValidMetadata, declared edges)` concurrently via
  a `JoinSet` (results indexed for deterministic ordering — quality-rust.md
  JoinSet rule). Cycles are impossible (digest-addressed), so BFS terminates.
- **Phase 2 — sequential visibility fold (pure, no I/O).** Compute each node's
  effective visibility from the root by folding `through_edge` down every path and
  `merge`-ing at diamonds — the **identical algorithm** as
  `ResolvedPackage::with_dependencies`, sourced from gathered metadata instead of
  installed `resolve.json`. Then build the `interface_surface` aggregate.

**Per-node fetch** (the offline-aware, cache-first step):

```
fetch_manifest(dep.identifier, IndexOperation::Resolve)   // digest-addressed → local-first every ChainMode; net-on-miss writes blobs, no tag
  ├─ Some(Manifest::Image(img))       → load_config_metadata(index, dep_pinned, &img)   // cache-first config blob (goals 4+5)
  ├─ Some(Manifest::ImageIndex(idx))  → select child by `platform`, fetch child manifest, load_config_metadata   // robustness for hand-authored index-pinned deps
  └─ None                             → is_offline() ? PolicyBlocked(81, hint) : NotFound(79)   // fail closed
```

`Op::Resolve` on digest-addressed content is exactly the existing default-inspect
routing (`resolve_top_manifest` uses `Resolve` deliberately): local-first, network
fetch + blob write-through on miss (goals 4 + 5), no tag-pointer commit (pinned-id
pull). Under `--offline` the client is absent (`is_offline()`), a digest miss is a
clean **policy block**, not a fault. Under `--frozen` the client is present and
digest content still fetches (only *unpinned-tag* resolution is frozen), so a
frozen closure over cached-or-fetchable digests works.

**Fail-closed:** any single node error aborts the *whole* closure for that package
(returns `Err(PackageErrorKind)`). A partial closure MUST NOT be rendered as a
complete one — an incomplete interface-surface aggregate would be a dangerous lie
("these are all the binaries" when more exist but couldn't be loaded). At the
batch layer, one package's closure error is that package's `inspect_all` entry
(via `drain_package_tasks`, input-order results / index-sorted errors); other
packages proceed.

*Named-and-deferred third option (panel W1):* a **marked-partial** mode — per-node
`unreachable: true` markers + a top-level `closure_complete: false` — would let a
wide closure with one flaky registry still show the 199/200 resolvable nodes,
consistent with the "couldn't determine ≠ determined zero" doctrine. Deferred, not
rejected: v1 ships whole-closure fail-closed (simplest honest contract; the
aggregate must stay fail-closed in ANY future mode). Revisit trigger: a real user
hits the flaky-wide-closure case.

**Component contracts (lib — `ocx_lib`):**

```rust
// crates/ocx_lib/src/package_manager/tasks/inspect.rs (extended)

/// A metadata-only dependency closure: the transitive set of packages reachable
/// from an inspected root, computed from config-blob metadata alone (no install).
pub struct InspectClosure {
    /// Flat, deduped node list in topological order (deps before dependents,
    /// root last). Diamonds appear once with the most-open merged visibility.
    pub nodes: Vec<ClosureNode>,
    /// Interface-surface binary claims: root unconditional, each dep iff its
    /// effective visibility `has_interface()`. Reuses the composer admission rule.
    pub interface_binaries: Vec<(oci::PinnedIdentifier, BinaryName)>,
    /// Interface-surface entrypoint names, same admission rule.
    pub interface_entrypoints: Vec<(oci::PinnedIdentifier, EntrypointName)>,
    /// `false` iff at least one interface-admitted node has UNDECLARED binaries
    /// (`ClosureNode.binaries == None`). Entrypoints are always complete.
    pub interface_binaries_complete: bool,
    /// Install/compose-gate conditions detected over the interface projection
    /// (Codex C2): entrypoint-name collisions + same-repo-two-digests. Empty =
    /// the surface is realizable. Detection is pure post-processing; reporting
    /// is non-fatal (view-not-gate; `deps`' warn_repo_digest_conflicts precedent).
    pub conflicts: ClosureConflicts,
}

pub struct ClosureConflicts {
    pub entrypoints: Vec<EntrypointConflict>,   // { name, packages: Vec<PinnedIdentifier> }
    pub repositories: Vec<RepositoryConflict>,  // { repository, digests: Vec<Digest> }
}

/// One node of the closure.
pub struct ClosureNode {
    pub identifier: oci::PinnedIdentifier,       // digest-addressed; advisory tag preserved for display
    pub effective_visibility: Option<Visibility>, // composed from the root; None iff is_root (axis undefined for the root)
    pub binaries: Option<Binaries>,              // tri-state, straight from the node's Bundle.binaries
    pub entrypoints: Vec<EntrypointName>,         // the node's declared entrypoint map keys
    pub dependencies: Vec<ClosureEdge>,          // the node's own declared edges
    pub is_root: bool,
}

/// A declared dependency edge (as authored), carrying its declared visibility.
pub struct ClosureEdge {
    pub identifier: oci::PinnedIdentifier,
    pub visibility: Visibility,                  // the DECLARED edge visibility (goal #2)
    pub name: DependencyName,
}

// Module-private free functions (not on the pub facade). Per the task-module
// rule they take EXPLICIT params — never the PackageManager facade (panel W2):
//   const CLOSURE_FETCH_CONCURRENCY: usize = 8;   // Phase-1 Semaphore bound (panel W5)
//   async fn walk_closure(index: &oci::index::Index, offline: bool,
//                         root_pinned: &PinnedIdentifier, root_metadata: &ValidMetadata,
//                         platform: &Platform) -> Result<InspectClosure, PackageErrorKind>
//   async fn gather_closure_nodes(index, offline, frontier, platform)
//       -> phase 1 (JoinSet bounded by CLOSURE_FETCH_CONCURRENCY, digest-deduped)
//   fn fold_effective_visibility(...)   -> phase 2 (pure; mirrors with_dependencies)
```

`InspectResult` grows the closure carrier; the facade takes a mode struct instead
of two interacting bools (panel S1 — `deps` implies selection on index roots, so
the pair is a mode, not independent switches):

```rust
pub enum InspectResult {
    Candidates { pinned, candidates },                              // never carries a closure (no metadata)
    Manifest   { pinned, metadata, layers, closure: Option<InspectClosure> },
    Resolved   { pinned, metadata, chain: Box<ResolvedChain>, closure: Option<InspectClosure> },
}

#[derive(Clone, Copy, Default)]
pub struct InspectOptions { pub resolve: bool, pub deps: bool }

pub async fn inspect(&self, package: &oci::Identifier, platform: oci::Platform,
                     options: InspectOptions) -> Result<InspectResult, PackageErrorKind>;
pub async fn inspect_all(&self, packages: Vec<oci::Identifier>, platform: oci::Platform,
                         options: InspectOptions) -> Result<Vec<InspectResult>, package_manager::error::Error>;
```

Inside `inspect`, when `options.deps`: the effective "needs platform selection" =
`options.resolve || (options.deps && root is ImageIndex)`; after the root's
`ValidMetadata` is loaded (existing Manifest/Resolved path), call
`walk_closure(self.index(), self.is_offline(), &pinned, &metadata, &platform)`
and attach `Some(closure)`.

*Implementation note (panel spec-4):* the root's manifest **shape** is only known
after a first fetch, so bare `--deps` on an index root fetches the top manifest
once via the no-selection path, then re-resolves via the existing `resolve()`
path to platform-select. Under `ChainMode::Default`/`Frozen` the second
round-trip hits the just-warmed local CAS — accepted redundancy, not a
correctness issue. Under `--remote` there is no local warm cache to hit and
this is a genuine second network fetch.

**Committed error variants (panel spec-3)** — the fail-closed walker returns
exactly these, so builder and tester cannot diverge:

- dep manifest/config absent under offline policy → `PackageErrorKind::Internal(crate::Error::OfflineMode)` → exit 81
- dep genuinely absent with a source consulted → `PackageErrorKind::NotFound` → exit 79
- malformed / wrong-media-type / over-cap config → the existing `load_config_metadata` errors (`Internal` → DataError) → exit 65

**Component contracts (CLI — `ocx_cli`):**

- `command/package_inspect.rs`: add `#[clap(long)] deps: bool` (flag before the
  positional); forward to `inspect_all(.., self.deps)`.
- `api/data/package_inspect.rs`: add `closure: Option<ClosureOut>` +
  `interface_surface: Option<InterfaceSurfaceOut>` to `PackageInspect`; serialize
  when present; render the closure tree (`(*)` dedup) + interface-surface branch in
  `print_plain`. Reuse `BinaryAttribution::from_pairs` for the aggregate arrays.

### Decision D4 — Tier split (extend-don't-duplicate ruling)

`ocx package deps` (installed-tier) and `ocx package inspect --deps`
(metadata/pre-install tier) are a **clean tier split, not a duplicate pipeline**:

| | `ocx package deps` | `ocx package inspect --deps` |
|---|---|---|
| Data source | installed `resolve.json` (pre-computed TC) | config blobs (cache-first, walked live) |
| Precondition | package installed | nothing — reads metadata only |
| Lifecycle | post-install introspection | pre-install planning |
| Shared | visibility vocabulary, `(*)` digest-dedup rendering idea | (same) |

They share **vocabulary and rendering idiom**, not a data path — the "extend, don't
duplicate" doctrine forbids a *parallel pipeline over the same source*, which this
is not. Keeping them separate is correct: forcing `deps` to answer the
uninstalled question would fork its contract on CWD/install-state, exactly the
layer-impurity the CLI rules forbid.

### Decision D5 — Config-blob size cap (scope: IN, small guard)

**Chosen: add a `MAX_METADATA_BLOB_BYTES` guard in the shared
`load_config_metadata` loader**, enforced in two steps (Codex C1 revision):

1. **Pre-fetch descriptor rejection** — the config descriptor's declared `size`
   is known from the already-fetched manifest BEFORE any blob request; an
   over-cap declared size is rejected with `DataError` **without touching the
   network or the cache**.
2. **Post-fetch length check** — the fetched byte length is re-checked before
   `serde_json::from_slice` (defends against a registry lying small in the
   descriptor and shipping big).

**Honest scope limit (Codex C1):** step 2 runs after the current `Client::pull_blob`
path has already buffered the body, so a malicious registry ignoring the
descriptor can still spend memory before the check fires. This ADR therefore does
NOT claim to close the CWE-400 class — it narrows it (step 1 blocks every
honestly-declared oversize without a fetch) and keeps the documented `DataError`
contract. Fully closing the class requires **byte-limited streaming reads at the
transport/client layer for both manifests and config blobs** — that is the
deferred client-layer hardening ticket (below), now explicitly widened to cover
both blob kinds. The walk multiplies N over an exposure that already exists on
every current fetch path; it does not create the class.
Config metadata is KB-scale in practice — that is the justification; a 4 MiB
ceiling is orders of magnitude above any real package and therefore never bites.
(The OCI spec's "≥4 MiB" figure is a *registry push-acceptance floor*, not a
client read-ceiling recommendation — panel sota-5 — so the cap does not claim
spec backing, only generous headroom.) Because the guard lives in the shared
loader, the existing single-manifest inspect and the pull pipeline inherit it too
(strict improvement, low risk).

**Out (deferred, widened per Codex C1):** byte-limited **streaming** reads at the
transport/client layer for BOTH manifest bodies and config blobs (cap enforced
while reading, before buffering or cache write-through). That read happens inside
`fetch_manifest`/`pull_blob`, below the walker's reach; it is the broader
client-layer hardening (the domain research's standalone gap) and gets its own
ticket, not this PR. This PR's D5 guard is the walker-reachable narrowing, not the
class fix.

### Decision D6 — No new store, no new persistence tier

**(Amended post-rebase onto `feat(index)!` / `adr_index_indirection`.)** The
original premise — read-path write-through persists manifests for free — no
longer holds: under the one-index-format architecture, the local index stores
**dispatch objects only** (A3), and leaf platform manifests are persisted solely
by install/pull's `stage_and_link_chain_blobs`. Config blobs still write
through the content store on fetch.

Resolution (owner goal 4 sanctions it verbatim — "loaded manifests/config may
be persisted in the local blob cache"): when `--deps` is set, the walk **stages
fetched leaf manifests into the content store itself** via
`inspect.rs::stage_leaf_manifest` — one shared staging fn used for both the
root's chain and every dep node, reusing the check-and-heal step factored out of
`stage_and_link_chain_blobs` as `common::blob_needs_fetch`. No ref-links are
created (inspect has no installed package dir): staged blobs are **unreferenced
cache entries**, reclaimable by `ocx clean` — cache semantics, so
warm→clean→offline failing is expected and correct. A plain `inspect` without
`--deps` performs zero new writes (main's read-paths-don't-persist design is
untouched). The index's dispatch store is never written from inspect — A3
intact.

Still true: no closure-to-disk artifact, no new store, no cache index — the
content-store blob cache *is* the persistence, and goals 4+5 are pinned by the
warm→offline unit + acceptance tests. Multi-writer `BlobStore` re-hash (PR #169
scope) stays out.

### Decision D7 — No `--self`, no schema change, no migration

- **No `--self` on inspect.** `--self` (private surface) is meaningful only for a
  package's *own runtime* — you never run an uninstalled package's private surface.
  Inspect is a **publisher/consumer-surface** view: "what does this package expose
  to me if I depend on it." The closure lists **all** nodes with their effective
  visibility (sealed/private included, for completeness), and the *aggregate* is
  interface-only — the sole meaningful pre-install answer. A private-surface
  aggregate would have no consumer semantics. Out of scope.
- **No new metadata field / no schema regeneration.** The closure is **derived**
  from existing fields (`binaries`, `dependencies`, `entrypoints`, `visibility`).
  `metadata.json`, `ocx_schema`, and the metadata docs page are untouched.
- **Migration: none.** Purely additive — a new flag and two additive JSON keys on
  the `Manifest`/`Resolved` inspect bodies. Existing consumers of inspect JSON are
  unaffected; the `Candidates` body is unchanged.

---

## UX Scenarios

| Scenario | Command | Outcome | Exit |
|---|---|---|---|
| Single-manifest root with deps | `ocx package inspect cmake@sha256:… --deps` | Manifest body + `closure` (flat) + `interface_surface`; plain shows the closure tree | 0 |
| Image-index root with deps | `ocx package inspect toolchain:1.0 --deps` | `--deps` platform-selects (host default); Resolved body + closure | 0 |
| Explicit platform | `ocx package inspect toolchain:1.0 --deps -p linux/arm64` | Closure computed against `linux/arm64` | 0 |
| Redundant `--resolve` | `ocx package inspect toolchain:1.0 --deps --resolve` | Same as `--deps` on an index root (resolution already implied) | 0 |
| Leaf package (no deps) | `ocx package inspect zlib@sha256:… --deps` | `closure` = [root]; `interface_surface` = root's own binaries/entrypoints | 0 |
| JSON for automation | `ocx --format json package inspect toolchain:1.0 --deps` | Flat `closure` array + `interface_surface` object | 0 |
| Multiple packages | `ocx package inspect a b --deps` | Object keyed by raw id; each value carries its own closure | 0 |
| Offline, closure warm in cache | `ocx --offline package inspect toolchain:1.0@sha256:… --deps` | Whole closure served from local CAS (goals 4+5) | 0 |
| Offline, a dep blob absent | `ocx --offline package inspect toolchain:1.0 --deps` | Fail closed — `PolicyBlocked`, hint to run online / `ocx package pull` | 81 |
| `--deps` on Candidates path | (image index, no `-p`) | `--deps` selects host platform anyway (never the metadata-less Candidates body) | 0 |

## Error Taxonomy

| Condition | Surfaced as | Exit |
|---|---|---|
| Root tag unknown (source consulted) | `NotFound` | 79 |
| Root unpinned tag miss under offline/frozen | `PolicyResolutionBlocked` (existing) | 81 |
| **Dep manifest/config blob absent under offline policy** | `PolicyBlocked` (walker; hint: run online / `ocx package pull`) | 81 |
| Dep genuinely not found online | `NotFound` | 79 |
| Root/dep config invalid or malformed metadata | `Internal` → `DataError` (existing `load_config_metadata` gate) | 65 |
| Dep config wrong media type | `DataError` (existing media-type gate) | 65 |
| **Metadata blob exceeds `MAX_METADATA_BLOB_BYTES`** | `DataError` (new D5 guard) | 65 |
| Dep image-index child: no platform match (child manifests exist, none compatible) | `FeatureMismatch` → `DataError` — deterministic, never 79 (panel W4; backend-first needs one predictable code) | 65 |
| Dep image-index names a child digest that is genuinely absent | `NotFound` | 79 |
| Corrupt child digest in a dep image index | `Internal(DigestError)` → `DataError` | 65 |

All classify through the existing `classify_error` free function; no new
`ExitCode` variant. Fail-closed: any of the above aborts the closure for that
package (never a partial render).

## Edge Cases

| Case | Resolution |
|---|---|
| **Diamond** (dep reached via two paths) | One `closure` node; effective visibility = `merge` of both paths (most open). Plain tree renders it once in full, `(*)` on the repeat visit (digest-keyed). |
| **Sealed-only closure** (root, all deps sealed) | All deps listed with `effective_visibility: sealed`; `interface_surface` = root's own binaries/entrypoints only. |
| **`binaries: None` vs `Some([])` on an interface node** | `None` (undeclared): node contributes no known names AND sets `interface_binaries_complete = false` ("couldn't determine ≠ determined zero"). `Some([])` (asserted zero): contributes nothing but keeps `complete` true. **Explicit ruling:** an undeclared-binaries interface dep makes the *binary* aggregate `incomplete`, not silently smaller. |
| **Entrypoints aggregate completeness** | Always complete — the entrypoint map keys are authoritative names, not an unverified claim; no `_complete` flag needed. |
| **Cycle** | Impossible by content-addressing (a digest cannot name itself); BFS terminates without a cycle guard, but the digest visited-set is the structural backstop. |
| **Dep pinned to an image index** (hand-authored) | Walker platform-selects the child (walk's platform), then loads config. No platform child → fail closed (`FeatureMismatch`/`NotFound`). Node identity = the **selected child's** digest — matches install-time resolution (`pull.rs`'s `info.identifier()` is likewise the platform-selected identity, never the index; Codex C1). The authored index reference stays visible, unchanged, on the parent's `ClosureEdge`. Two declared edges that resolve to the SAME digest (e.g. a direct pin and an index pin whose selected child coincides) collapse to one closure node — claims counted once, no false same-repo-two-digests conflict. |
| **Root is a single manifest** | No platform selection; `-p` ignored (matches current Manifest mode). |
| **Same repo, two digests in the closure** | Both nodes appear (distinct digests → distinct nodes); inspect stays exit 0 (view, not gate) but the condition is surfaced in `interface_surface.conflicts.repositories` when both digests are on the interface projection (Codex C2). Sealed/private edges excluded from the scan — same exclusion as `collect_repo_digest_conflicts`. |
| **Duplicate interface entrypoint name across admitted nodes** | Surfaced in `interface_surface.conflicts.entrypoints` with all owners (Codex C2); exit 0. Install would reject this closure (`check_entrypoints`) — the report is what makes the aggregate honest about unrealizability. |
| **Advisory tag on a dep edge** | Preserved on `ClosureEdge.identifier` for display; dedup uses `strip_advisory()`. |

## Trade-off Analysis

- **Reuse over reinvention.** The walk reuses `fetch_manifest`+`load_config_metadata`
  (cache-first path, goals 4+5) and the `through_edge`/`merge` algebra
  (`with_dependencies`' exact logic). New code is the DAG gather + fold + the
  `api/data` projection — no new subsystem, no new store, no schema change.
- **Fail-closed vs partial answers.** Choosing fail-closed makes a transient
  registry hiccup or one corrupt dep abort the closure. That is the correct
  trade: a silently-partial interface surface is worse than a clean error, because
  the aggregate's entire value is being *authoritative* about "all interface
  binaries."
- **Flat JSON vs tree JSON.** Flat costs a small amount of consumer-side
  reconstruction to render a tree, but is the only diamond-safe shape and matches
  the cargo-metadata prior art the ecosystem already understands. Unlike cargo,
  OCX keys nodes by the **immutable content digest** rather than an opaque
  compound id string — cargo's `id` format change (rust-lang/cargo#12914) broke
  downstream tools that had come to rely on its shape (mozilla/cargo-vet#602);
  digest keying is structurally immune to that breakage class (panel sota-2).
- **Parallel gather.** Two-phase (parallel I/O + sequential pure fold) buys
  concurrency for the network-bound step while keeping the visibility merge — which
  is inherently sequential/order-sensitive — simple and correct. For tiny closures
  it is no slower than a sequential walk; for wide ones it wins.

## Documentation & Config Surfaces to Update

- `website/src/docs/reference/command-line.md` — `package inspect --deps` flag +
  the `closure` / `interface_surface` JSON shape and the plain tree.
- `crates/ocx_cli/src/command/package_inspect.rs` — clap `///` help for `--deps`
  (user contract only; no ADR/§ references — `quality-cli-help.md`). **Must also
  REVISE two now-stale existing sentences (panel spec-2; stale help = Block-tier
  once shipped):** (a) the command-level "`-p/--platform` applies only with
  `--resolve`" (line ~31) — `-p` now also applies with `--deps`; (b) the
  `--resolve` flag doc "Without this, an image-index reference lists its platform
  candidates instead" (line ~42) — no longer unconditionally true under `--deps`.
- `crates/ocx_cli/src/api/data/package_inspect.rs` — doc comments describing the
  closure/interface-surface plain + JSON shapes.
- `.claude/rules/subsystem-cli-commands.md` — `package inspect` row (`--deps` in
  Key Flags; closure/interface-surface note).
- `.claude/rules/subsystem-package-manager.md` — `tasks/inspect.rs` row (closure
  walker + `InspectClosure`/`ClosureNode`/`ClosureEdge`); parallel-task list
  (closure gather via JoinSet).
- **No `subsystem-metadata-schema.md` / `ocx_schema` / `metadata.md` change** — the
  closure is derived, no new metadata field (state explicitly in the plan).
- `product-context.md` — **flagged, not edited** (see Summary): a possible
  refinement note (pre-install interface enumeration reinforces #1/#2/#8), but no
  positioning *shift*; leave to owner.

## Test Strategy Skeleton

**Unit (lib, `#[cfg(test)]` with `TagStore`/`BlobStore` temp fixtures — the
existing `tasks/inspect.rs` spec-test harness):**

- Walker over a seeded 3-node chain (root → cmake(sealed) → zlib(public)):
  assert node set, topological order, effective visibilities
  (`through_edge`/`merge`), and the `interface_surface` aggregate.
- Diamond fixture: shared dep reached via two edges → one node, merged visibility,
  aggregate admits it once.
- Fail-closed: a dep config blob absent under an `Offline` manager → the whole
  closure errors `PolicyBlocked` (not a smaller closure).
- `binaries` tri-state aggregation: an interface dep with `None` binaries →
  `interface_binaries_complete == false`; with `Some([])` → stays `true`.
- Sealed-only closure: aggregate == root's own claims.
- Conflicts (Codex C2): fixture with two admitted nodes claiming the same
  entrypoint name → `conflicts.entrypoints` lists the name with both owners;
  fixture with one repo at two interface digests → `conflicts.repositories`
  entry; sealed-edge variant of the same repo pair → NO conflict entry
  (exclusion parity with `collect_repo_digest_conflicts`); clean closure →
  both arrays empty. Exit 0 in all four.
- Goals 4+5 pins: after a walk against a source-backed manager, the config/manifest
  blobs are present in the local CAS (write-through); a subsequent `Offline` walk
  succeeds purely local.
- Size cap, both D5 steps separately: over-cap DECLARED descriptor size →
  `DataError` with zero fetch observed on the mock source; honest descriptor +
  over-cap FETCHED body → `DataError` post-fetch.
- Invariant re-assertion (panel sota-4): the Phase-1 gather fan-out constructs CAS
  paths only from post-`Digest`-parse values — a test seeds a dep edge whose
  digest fails `Digest` parsing and asserts the walk errors *before* any path
  construction (durable statement of the digest-parse-before-path invariant for
  `gather_closure_nodes`).

**Acceptance (pytest, `test/tests/`):**

- Push a deps-bearing fixture (root + public + sealed deps, with `binaries` +
  `entrypoints`); `ocx package inspect <root> --deps --format json` → assert
  `closure` node set + `interface_surface` (binaries/entrypoints + attribution +
  `binaries_complete`).
- **Offline-after-warm** (the goals-4+5 acceptance): inspect `--deps` online to
  warm the cache, then `--offline` inspect `--deps` succeeds with an identical
  closure (zero network).
- Exit codes: offline + un-warmed dep → 81; malformed → 65.
- Plain-mode: closure tree renders with `(*)` on a diamond repeat.
- **`--self` NOT applicable** — no acceptance for it (justified: publisher-surface
  view; D7).

---

## Consequences

**Positive**

- Answers "what lands on my PATH if I install this" **before** installing —
  reinforces offline-first (#2) and backend-first (#1) and the declarative-env
  differentiator (#8), from cache-first reads.
- Zero new subsystem/store/schema; reuses the cache path and the visibility algebra.
- Pins goals 4+5 with tests instead of re-plumbing.
- Out of scope, named for traceability (panel sota-1): the OCI 1.1 **referrers
  API** is the other standardized metadata-only enumeration surface
  (SBOM/signature discovery) — orthogonal to this dependency-closure walk, not a
  substitute; separate future ticket per `research_oci_metadata_cache_tech.md`.

**Negative**

- `--deps` on an image-index root silently platform-selects (documented) — a small
  surprise vs the metadata-less `Candidates` default.
- Fail-closed means one bad dep aborts a package's closure — correct, but stricter
  than a best-effort tool.

**Risks**

- Config-blob cap could in theory reject a pathological (multi-MiB) legitimate
  config — mitigated by a generous 4 MiB ceiling (real configs are KB-scale) and a
  clear `DataError` message.

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-07-20 | architect worker | Initial draft (Proposed) |
