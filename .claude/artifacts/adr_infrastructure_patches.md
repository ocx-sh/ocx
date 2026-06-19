# ADR: Infrastructure Patches — Site Packages via Compose-Time Overlay

<!--
Architecture Decision Record — MADR format.
Owner: Architect (/architect). Handoff: Builder (/builder), Security Auditor (/security-auditor), QA (/qa-engineer).
-->

## Metadata

**Status:** Proposed
**Date:** 2026-06-19
**Deciders:** @michael-herwig, architect
**GitHub Issue:** part of milestone #111 (Corporate infrastructure v1); sub-issues #112–#117
**Supersedes:** the `feature/patches` draft `adr_infrastructure_patches.md` (2026-03-22), which modelled patches as `EdgeKind`/Kahn graph edges. That graph model never landed on `sion` — it was overtaken by the **two-env composition** refactor (`adr_two_env_composition.md`). This revision re-bases the patch design onto the shipped compositional model.
**Tech Strategy Alignment:**
- [x] Rust 2024, Tokio, no new runtime deps. Reuses config (`config/mirror.rs` precedent), OCI client, file-structure CAS, GC reachability.
**Domain Tags:** infrastructure, integration, security, package-manager

---

## Context

Infrastructure operators must layer **site-specific configuration** (corporate CA bundles, proxy/mirror URLs, license server endpoints, truststores) onto **unmodified upstream packages** they do not publish and cannot push to. The mechanism is a **patch**: client config points at an operator-controlled *patch registry*; a well-known `__ocx.patch` descriptor maps an upstream package to a set of **companion packages** whose env vars overlay onto the base package's environment.

This was first designed on `feature/patches` (2026-03-22). Since then two things changed on `sion`:

1. **The composition model was rewritten** (`adr_two_env_composition.md`). There is no dependency *graph* walked at exec time, no `EdgeKind`, no Kahn topological sort, no `FlatEntry`. Instead each package stores a **flat transitive closure (TC)** in `resolve.json` with **pre-computed effective `Visibility`** (two axes: `interface`/`private`), built inductively at install via `ResolvedPackage::with_dependencies` (`crates/ocx_lib/src/package/resolved_package.rs:60`). Exec-time `composer::compose(roots, store, self_view)` (`crates/ocx_lib/src/package_manager/composer.rs:66`) does a single flat iteration over each root's TC, gating entries by `has_interface()`/`has_private()`. The old ADR's entire Phase 3–5 premise is dead.

2. **The OCI registry mirror feature shipped** (`adr_oci_registry_mirror.md`). Read-path pulls now route through `MirrorMap` (`crates/ocx_lib/src/oci/client/mirror_map.rs`). The old ADR predates it.

**The reframe (requester's direction, adopted):** *A patch is nothing but a **site-loaded package*** — a regular OCX package, installed from an operator-controlled tier, whose **interface environment is composed into the execution environment** of the package it patches. There is no new artifact-composition algebra; patches reuse the existing two-env composer. The only genuinely new machinery is (a) **discovering** which site packages apply to a given base identifier, and (b) an **update path** that refreshes those site packages **independently of the base package**, offline.

### What already exists (do not rebuild)

| Capability | Anchor | Reuse for patches |
|---|---|---|
| Two-env composer (flat TC, surface gating) | `package_manager/composer.rs:66` | Companion env emission + visibility tiering |
| Effective `Visibility` (interface/private) | `package/metadata/visibility.rs`; `resolved_package.rs:60` | Patch inherits the visited TC entry's surface |
| Config tier-merge + per-host resolver | `config/mirror.rs`, `resolve_mirror_map` (`lib.rs:13`) | `PatchConfig` + `resolve_patch_config` follow this exactly |
| OCI artifact tag pull (`__ocx.desc`) | `oci/client.rs:1030` (`pull_description`) | Template for `__ocx.patch` pull (but must persist — see below) |
| CAS stores + `ReferenceManager` back-refs | `file_structure.rs:40`, `reference_manager.rs` | Descriptor blob + companion install lifecycle |
| GC reachability BFS (`refs/{symlinks,deps,layers,blobs}`) | `package_manager/tasks/garbage_collection/reachability_graph.rs` | Companion + descriptor survive GC |
| Internal-tag hiding (`__ocx.` prefix) | `package/tag.rs` (`Tag::is_internal_str`) | `__ocx.patch` auto-hidden, zero work |
| Resolution-config forwarding to child ocx | `env.rs:248` (`apply_ocx_config`, `OcxConfigView`) | Add `OCX_PATCHES` so launchers re-apply patches |

---

## Decision Drivers

The six load-bearing requirements (R-numbers used throughout):

| # | Requirement |
|---|---|
| **C1** | **Compositional load.** A site package's *interface* environment composes into the *root execution environment*, overriding the publisher's values for the same key (infra intent wins). |
| **C2** | **Entry-point tiering.** Entry points run in their own composed environment (`self_view=true`) and must inherit the root's patches; entry-point-owned packages may carry their own patches. |
| **C3** | **Visibility tiering.** A patch for a dependency that reaches the root only on the *private* surface must load **only** into the private surface. Patches inherit the interface/private visibility of the target they patch. |
| **C4** | **Independent offline update.** Patches churn (URL change, license renewal) far faster than packages. Refreshing a patch must (a) work offline at exec time once synced, (b) **not** require re-installing the base packages in use — patch identity is decoupled from base-package digest. |
| **C5** | **Loading guarantee.** Every applicable patch is *always* loaded when its target loads — including patches for transitive deps, including encapsulated entry-point deps needing the patch on their private surface. No silent skips. |
| **C6** | **CAS persistence + GC.** Patch descriptors and companion packages persist in the blob/layer CAS so the offline contract holds (zip `OCX_HOME` → unzip elsewhere → reinstall → identical patched env), and GC must keep them reachable. |

---

## Considered Options

The *source* decision (where patch descriptors live) was settled by the original ADR and is **retained**: **client-configured patch registry** (operator-owned OCI registry, `[patches]` config, `__ocx.patch` well-known tag). Options 1/2/4 (referrers API, source-registry tag, publisher metadata) all require push access to registries the operator doesn't control, or publisher cooperation — rejected as before.

What this revision decides is the **composition + persistence model**. Two viable options:

### Option A — Install-time TC injection ("site packages as TC entries")

At install, after building the base package's declared-dependency TC (`pull.rs:431`), resolve its patches and **inject the companion packages into the `with_dependencies` iterator** as additional edges. They flow through the `through_edge`/`merge` visibility algebra, land in `resolve.json`, and the composer needs **zero change**. GC works for free (companions are TC entries → walked like any dep). Visibility tiering (C3) is free (companion inherits the target edge's visibility).

**Fatal flaw vs C4.** `resolve.json` lives inside the base package's **content-addressed install directory** (`packages/{registry}/{repo}/sha256/.../resolve.json`). Baking patches into it means: (1) a patch update requires rewriting a CAS object's contents — violates immutability and the offline/portability digest contract (R4 of the mirror ADR's lineage); or (2) re-installing the base package to regenerate `resolve.json`. Both violate C4 ("don't update the base packages in use"). Also, patch config is *global client config*, not publisher metadata — folding it into a per-install immutable artifact couples two lifecycles that must stay independent.

### Option B — Compose-time site overlay ("site layer") **(RECOMMENDED)**

Keep base-package TCs **pure** (publisher-declared deps only). Maintain a **separate, mutable, independently-synced site layer**: for each base identifier, a persisted `__ocx.patch` descriptor (CAS blob) + the companion packages it names (regular installed packages) + a mutable pointer (`artifacts/{patch_registry}/{repo}/patch` symlink + `tags/` index entry). At compose time the composer, **for each TC entry it already visits** (and for the root), consults the site layer for that identifier and emits the matched companion packages' interface env **gated by the same surface boolean** the TC entry resolved to.

- **C1** ✓ companion interface vars appended after the base's own vars → infra overrides publisher by append-order (Constant replaces, Path prepends).
- **C3** ✓ companion env emitted under the *visited TC entry's* `want` (`has_interface()`/`has_private()`); a private-only dep's patch lands only on the private surface, for free.
- **C4** ✓ the site layer is re-synced independently (`ocx patch update`); base `resolve.json` is never touched; companion packages are their own CAS installs addressed by their own digest.
- **C5** ✓ the composer visits *every* TC entry + root on the active surface; each visit does a site-layer lookup → no patch skipped, including private entry-point deps under `self_view=true`.
- **C6** ✓ descriptor persisted as a real CAS blob; companions are normal packages; both GC-anchored (see GC section).

**Cost:** a real (small) composer change + a persisted descriptor (closing the `pull_description` in-memory gap) + the new independent-update path. This is the only option that satisfies C4, so it wins despite costing more than A.

### Weighted comparison

| Criterion (weight) | A: install-time TC | B: compose-time overlay |
|---|---|---|
| Satisfies C4 independent update (35%) | ✗ fails | ✓ |
| Visibility tiering C3 (20%) | ✓ free | ✓ via visited-entry surface |
| Composer change size (15%) | ✓ none | ~ one overlay pass |
| GC reuse C6 (15%) | ✓ free | ✓ descriptor+companion anchored |
| Offline portability C6 (15%) | ✗ mutating CAS | ✓ |
| **Score** | rejected on C4 | **chosen** |

---

## Decision Outcome

**Chosen: Option B — compose-time site overlay, with companion env inheriting the visited TC entry's effective `Visibility`.** Patches are site-loaded packages composed by the existing two-env composer through a thin overlay step; their lifecycle (sync, update, GC) is independent of the base package's install.

### The overlay mechanism (concrete)

`composer::compose` (`composer.rs:66`) today, per root, does: (1) iterate the root's TC, surface-gate each entry, emit its interface vars (`emit_interface_vars`, `composer.rs:343`); (2) emit the root's own vars (`emit_root_vars`, `composer.rs:354`); (3) emit entrypoint synth-PATH. **Crucially, root vars are emitted *after* all dependency vars**, and env application is last-wins (`Modifier::Constant` replaces, `Modifier::Path` prepends — `env.rs:292`). So an overlay interleaved *per visited entry* would be overridden by the root's own later-emitted vars — a root-declared `SSL_CERT_FILE` would clobber a transitive dep's CA patch, violating C1. The overlay therefore runs as a **distinct second phase, globally after all publisher env**:

```
PHASE 1 — base compose (unchanged): for each root, emit TC-entry interface vars,
          then root's own vars, then entrypoint synth-PATH. Record, per root, the
          set of visited identifiers admitted on the active surface (the `want`=true set).

PHASE 2 — site overlay (new), appended AFTER all Phase-1 output:
  for each admitted identifier I (TC entries + roots, in deterministic order):
      for companion in site_layer.patches_for(I):   // descriptor rules matched against I's tag
          emit companion's INTERFACE vars only       // never the companion's private surface
```

Two invariants make this correct:

- **Global-last ordering (C1).** Every companion var is emitted after *all* publisher vars (deps and roots), so infra always overrides publisher for shared keys, regardless of whether the patched package is a deep transitive dep or the root. Deterministic companion order: targets in Phase-1 visit order, then descriptor rule order, then companion-list order.
- **Interface-only projection (no private leak).** A companion is emitted exactly like a dependency — only its `has_interface()` vars cross into the target env (`emit_interface_vars` semantics, `composer.rs:343`). The **target's** surface (`want`) decides *whether* the companion block is emitted; it never decides *which of the companion's surfaces* is read. Even under a launcher's `self_view=true`, the companion contributes only its interface vars — its own private env / private deps stay sealed. (This corrects an earlier "companion as visibility-inherited sub-root" framing, which would have leaked the companion's private surface under `self_view=true`.) A companion's *own* transitive deps contribute through the companion's interface projection, resolved when the companion was installed.

`site_layer.patches_for(I)` resolves entirely from local state (descriptor symlink under `artifacts/`, companion installs under `packages/`) — no network at compose time.

**Why "the visited entry's surface gates the block" is the whole of C3.** The composer already computed, per root, whether identifier `I` reaches the interface or private surface (`composer.rs:122`). The patch is *for* `I`, so its block is emitted iff `I` was admitted — a private-only dep's patch is therefore present only under `self_view=true` and absent from the default interface env. No new visibility algebra: the gate reuses the boolean the composer already has; the projection reuses the interface-only dep emission.

### Entry-point inheritance (C2)

Entry-point launchers call `manager.resolve_env(&[pkg], self_view=true)` (`crates/ocx_cli/src/command/launcher/exec.rs:54`) — there is **no separate entry-point environment**; the launcher composes the owning package with the private surface. Therefore:
- Root patches are inherited by entry points automatically: same `compose`, `self_view=true` visits the full TC, overlay applies.
- A dep that is private to the package but patched gets its patch on the private surface inside the entry point (C3 + C5 together).
- "Entry-point-owned patches" = patches whose descriptor targets the entry-point-owning package's identifier; they are in that package's TC, so they overlay like any other.

No per-entry-point patch config is introduced (the launcher model has no per-entry env to attach it to). If future need arises, it is additive.

### Independent update mechanism (C4) — the new work

This is the capability the old ADR lacked. Design:

- **Immutable vs mutable split.** The descriptor *manifest+layer* and each companion *package* are immutable content-addressed CAS objects (one digest per version). What is **mutable** is the *pointer*: the `__ocx.patch` tag → descriptor-digest mapping in the `tags/` index, and the `artifacts/{patch_registry}/{repo}/patch` symlink → current descriptor content. Updating patches re-points these without touching any base-package install.
- **`ocx patch update [<pkg>|--all]`** (and a piggyback on `ocx index update`): for each configured/known base repo, re-resolve the patch repo (via the `[patches]` path template), fetch the `__ocx.patch` tag, and if the digest advanced: persist the new descriptor blob, install any newly-named companion versions, re-point the `artifacts/.../patch` symlink, and update back-refs. The base package's `resolve.json` and install dir are **never rewritten**.
- **Decoupled from base digest.** The site layer is keyed by base **identifier** (`registry/repo` + tag-glob), not base **digest**. A base package pinned at digest X keeps running; only the overlay refreshes. A company that re-points `zscaler-root:latest` to a new cert bundle: `ocx patch update` pulls the new companion digest, re-points; next `ocx exec` composes the new value; the JDK install is untouched.
- **Offline at exec.** Once synced, compose reads only local state. Cold + offline + patches-configured + never-synced → hard error (patches are integral; see three-state index). Warm + offline → full success.

### Settled open questions

1. **`artifacts/` store name.** Adopt `artifacts/` as the 8th `FileStructure` store (confirmed free; `packages/`, `tags/`, `symlinks/`, `state/`, `temp/`, `blobs/`, `layers/`, plus the `projects/` ledger exist — `artifacts/` does not collide). Generic enough to later host `desc`/`sbom` pointers. Layout: `artifacts/{registry_slug}/{repo}/patch` symlink → descriptor content. (Follow-up: `subsystem-file-structure.md` documents only 6 stores and omits `state` — stale; correct it when `ArtifactStore` lands.)
2. **Descriptor CAS persistence.** **Required.** `pull_description` (`oci/client.rs:1030`) reads layer blobs into memory and never persists them — that gap is acceptable for READMEs but **not** for a patch descriptor, which must be a GC-anchored on-disk object for the offline contract. The `__ocx.patch` pull path persists via `BlobStore::write_blob` + `ReferenceManager::link_blobs`, mirroring the normal manifest-chain pull (`local_index.rs` `persist_manifest_chain`).
3. **Mirror interaction.** The patch registry is treated as any other registry: **subject to `[mirrors]` rewriting** (no special exemption). In practice the patch registry is an internal host rarely present in the mirror map, but exempting it would be a surprising special case; consistency wins. The operator owns both `[patches]` and `[mirrors]`.
4. **`ConstantTracker` role.** Under two-env compose, override resolution is **append-order last-wins** (`Modifier::Constant` replaces, `Modifier::Path` prepends) — the overlay appends companion vars after the base, so "patch overrides publisher" is intentional and automatic, needing **no** `EdgeKind`-aware tracker. Per-var info/warn conflict reporting is demoted to a **display concern** surfaced by `ocx env --show-patches` annotations, not a composition gate. This deletes the bulk of the old Phase 4. (`ConstantTracker` at `package/metadata/env/conflict.rs` stays as-is for the CI flavors that already use it.)

### Config forwarding (C5 across process boundaries)

Add `patches: Option<PatchConfig>` (resolved form) to `OcxConfigView` (`env.rs:72`) and an `OCX_PATCHES` key in `apply_ocx_config` (`env.rs:248`), forwarded as JSON like `OCX_MIRRORS`. Generated launchers re-enter `ocx launcher exec`, which must resolve the **same** patch layer or an entry point would silently lose patches — so forwarding is mandatory, decided in Phase 1 even though consumed in Phase 3+.

### Consequences

**Positive:** reuses the two-env composer (no graph machinery); visibility tiering is free; patch updates decouple cleanly from base installs; offline/GC reuse existing CAS + reachability; old Phases 3–5 shrink (no `EdgeKind`, no Kahn, no `ConstantTracker` rework).
**Negative:** a genuinely new independent-update path (`ocx patch update`, three-state sync) that the old ADR hand-waved; descriptor CAS-persistence is new code (closes the `pull_description` gap); composer gains an overlay pass touching a hot path.
**Risks:**

| Risk | Mitigation |
|---|---|
| Patch registry cold + offline + patches configured | Three-state index: never-synced → hard error ("patch index not synced"); patches are integral, not optional |
| Companion tag drift (`:latest` re-pointed) | `ocx patch update` is the intended refresh; document pinning companion tags; descriptor may pin by digest |
| Composer hot-path cost of per-entry site lookup | Lookup is a local map probe keyed by identifier; site layer loaded once per compose, not per entry |
| Forgotten forwarding → entry points lose patches | `OCX_PATCHES` forwarding is a Phase-1 acceptance criterion + a Block-tier review item (mirrors the `OCX_MIRRORS` rule) |

---

## Technical Details

### Configuration (`[patches]`) — unchanged from old ADR, still valid

```toml
[patches]
registry = "internal.company.com/ocx-patches"
path = "{registry}/{repository}"   # default; {registry} slugified via to_relaxed_slug()
# ocx.sh/java:21 → internal.company.com/ocx-patches/ocx_sh/java:__ocx.patch
```

`PatchConfig { registry: Option<String>, path: Option<String> }` mirrors `config/mirror.rs:26`; `resolve_patch_config(&config, env)` mirrors `resolve_mirror_map`; both re-exported from `lib.rs`. The placeholder tripwire test `parse_unknown_future_patches_section_silently_ignored` (`config.rs`) flips to positive parse tests.

### Patch descriptor (`__ocx.patch`)

OCI ImageManifest, `artifactType: application/vnd.sh.ocx.patch.v1`, single layer `application/vnd.sh.ocx.patch.descriptor.v1+json`:

```json
{ "version": 1, "rules": [
  { "match": "*",  "packages": ["internal.company.com/certs/zscaler-root:latest"] },
  { "match": "21", "packages": ["internal.company.com/java-config/jdk21-truststore:1.0"] }
] }
```

`PatchDescriptor { version: u32, rules: Vec<PatchRule> }`, `PatchRule { match_pattern: String, packages: Vec<Identifier> }`. Rules matched against the base tag (glob via the `glob` crate, already a dep); matches accumulate (union, dedup by identifier, first wins). Companion packages are ordinary OCX packages (env-only or content-bearing, e.g. a CA bundle exposing `SSL_CERT_FILE`).

### `ArtifactStore` + filesystem layout

8th `FileStructure` store (`file_structure.rs:65`, follow the `XyzStore::new(root.join("artifacts"))` pattern), CAS-sharded like the others:

```
$OCX_HOME/
  artifacts/{patch_registry_slug}/{repo}/patch     # → descriptor content; roots the descriptor via its refs/symlinks/
  packages/internal.company.com_ocx-patches/.../    # descriptor object: content/descriptor.json + refs/deps/→companions + refs/blobs/
  packages/internal.company.com/certs/zscaler-root/ # companion package content + refs/ (anchored by descriptor refs/deps/, NOT by a user install symlink)
```

A patch companion is pulled like a package but is **not** placed under `symlinks/` (no `candidates/`/`current`) — it is not user-selected; its only root is the descriptor's `refs/deps/` edge. If an operator *also* runs `ocx install <companion>` explicitly, that adds an independent install-symlink root, which is fine and orthogonal.

### GC reachability (C6)

Reuses the BFS in `reachability_graph.rs` — no new algorithm — but **the companion is anchored through the descriptor, never through the base install symlink.** An earlier model rooted a companion by writing a `refs/symlinks/` back-ref to the base's `candidates/<tag>` symlink; that is unimplementable with the current `ReferenceManager` contract: `link()` owns *both* sides and would repoint the forward (base install) symlink at the companion's content, corrupting the base install (`reference_manager.rs:64`). And any live `refs/symlinks/` target is treated as a GC *root* (`reachability_graph.rs`), so a hand-written companion back-ref would root the companion permanently. The correct model uses only the two existing edge kinds:

- **The descriptor is the single GC anchor for its companions.** The descriptor is persisted as a package-shaped object and holds a forward `refs/deps/{digest}` → each companion's content (`ReferenceManager::link_dependency`, which creates *no* back-ref — exactly the dependency model). The descriptor itself is rooted by the `artifacts/{patch_registry}/{repo}/patch` symlink: that symlink's `link()` writes a `refs/symlinks/` back-ref on the descriptor object, making the descriptor a root **iff** patch config currently references that repo.
- **Lifecycle (config-driven, independent of base install — reinforces C4).** Companions live as long as their descriptor is rooted, i.e. as long as `[patches]` resolves that repo and the `__ocx.patch` tag matches. Removing `[patches]`, or the repo ceasing to match, removes the `artifacts/.../patch` symlink → descriptor loses its only root → on `ocx clean` the descriptor is collected, its `refs/deps/` edges drop, and any companion not referenced by another live descriptor (or an explicit user `ocx install`) is collected with its layers/blobs via the normal BFS. Companions are therefore **not** tied to whether a base package is currently installed — a deliberate choice: the site layer's lifetime is the *config's* lifetime, matching the independent-update model.
- **`ocx patch update` and old companion digests.** When an update re-points a descriptor to a new digest, `link_dependency` updates the descriptor's `refs/deps/` to the new companion digests; the previous descriptor object loses its `artifacts/.../patch` root (symlink re-pointed to the new descriptor content) and becomes collectible, and any companion digest no longer referenced by the new descriptor falls out of reachability on the next `ocx clean`. No dangling roots, no manual ref bookkeeping outside `ReferenceManager`.
- **Descriptor blob persistence** (the `pull_description` gap): the `__ocx.patch` manifest + layer blobs are written via `BlobStore::write_blob` and linked via `link_blobs`, so they sit in the descriptor object's `refs/blobs/` and survive GC like any package's manifest chain.

If a future requirement genuinely needs companion lifetime tied to *base-install* presence (collect the CA bundle the moment the last patched JDK is uninstalled, even with config intact), that requires a **first-class `refs/patches` edge** with explicit `ReachabilityGraph` + `ReferenceManager` support — explicitly out of scope for v1, recorded here so the choice is visible.

### Three-state patch index (C4/C5 offline contract)

Reuse `tags/{patch_registry}/{repo}.json`:

| State | File / key | Meaning |
|---|---|---|
| Never synced | file absent | Patch repo never checked. Offline + patches configured → **hard error**. |
| Synced, no patch | file present, `__ocx.patch` absent | Valid; not every base has a patch. Skip silently. |
| Synced, has patch | file present, `__ocx.patch` → digest | Resolve descriptor + companions. |

`ChainMode::Offline` (`oci/index.rs`) already blocks unpinned-tag resolves with `PolicyBlocked` (exit 81); the never-synced patch case maps to the same contract. Zipping a warm `OCX_HOME` carries descriptors + companions + tag state → fully self-contained offline.

---

## Implementation Plan (re-derived under Option B)

Mapping to issues #112–#117. **Shrinks** vs the old graph plan are marked ↓; **new** work from C4 is marked ✚.

### Phase 1 — `[patches]` config + forwarding (#112)
- `PatchConfig` + `resolve_patch_config` (mirror `config/mirror.rs`), `Config` field + merge, `lib.rs` re-export, path-template expansion (`to_relaxed_slug`), `Context` wiring.
- ✚ `OCX_PATCHES` in `OcxConfigView` + `apply_ocx_config` (forwarding decided now).
- Flip the placeholder tripwire test to positive parse/expansion tests.
- *Validity under reframe: unchanged and confirmed.* Low risk.

### Phase 2 — Descriptor types + persisted `__ocx.patch` pull + `ArtifactStore` (#113)
- `PatchDescriptor`/`PatchRule` serde types; glob matching.
- `ArtifactStore` (8th store) + `artifacts/.../patch` symlink via `ReferenceManager::link`.
- ✚ **Persist** descriptor blob (`BlobStore::write_blob` + `link_blobs`) — close the `pull_description` in-memory gap. Slightly bigger than the old plan.
- `__ocx.patch` auto-hidden already (no work).

### Phase 3 — Site-layer resolution + companion install (#114) ↓
- `SitePatchResolver`: base identifier → descriptor → matched companion identifiers (union/dedup).
- Install companions as regular packages; create back-refs (descriptor `refs/deps/` → companion; companion `refs/symlinks/` ← base install symlink).
- **No `EdgeKind`, no Kahn, no graph.** Large shrink vs old Phase 3.

### Phase 4 — Compose-time overlay + visibility tiering + display (#115) ↓
- Add the **Phase-2 overlay** to `composer::compose`: Phase 1 records the admitted-identifier set per active surface; Phase 2 appends, globally last, each matched companion's **interface-only** vars (gate = target admitted; projection = interface). Covers root + entry-point (`self_view=true`) paths.
- `ocx env`/`ocx deps` `[patch]` annotations + `--show-patches`.
- **`ConstantTracker` rework dropped** (append-order last-wins handles overrides). Large shrink.

### Phase 5 — Independent update + lifecycle + GC + three-state (#116) ✚↓
- ✚ `ocx patch update [<pkg>|--all]` + piggyback on `ocx index update`; mutable-pointer re-point without base reinstall.
- Three-state index handling (never-synced offline → error; synced-no-patch → skip).
- `ocx uninstall` removes companion back-refs; verify `ocx clean` collects orphan companions + descriptors (no new GC code).
- GC reuse confirmed by test, not new algorithm.

### Phase 6 — Acceptance tests + docs (#117)
- `test/tests/test_patches.py`: compose order + override; visibility tiering (private-only dep patch absent on interface, present under `--self`); entry-point inheritance; **independent update** (re-point companion `:latest`, no base reinstall, env changes); offline zip/unzip parity; GC collect/retain; three-state index.
- Docs: user-guide "Patching packages for your infrastructure"; `[patches]` + `ocx patch update` reference; cross-link env-composition page; schema regen.

---

## Validation

- [ ] Configure patch registry, install base, `ocx exec` shows companion-overridden env in correct order (C1).
- [ ] **Global-last ordering:** a patch on a *transitive dependency* overrides a `Constant` **and** a `Path` var that the **root** package itself declares — patch wins (C1; the Phase-2 regression Codex flagged).
- [ ] **Interface-only projection:** a companion carrying a private-only env var (and a private-only dep) never appears in the patched target's env, even when the target is reached under `--self`/launcher `self_view=true` (no private-surface leak).
- [ ] Private-only dep's patch absent from default (interface) env, present under `--self` (C3); entry-point launcher inherits root patches (C2/C5).
- [ ] **GC anchoring:** companion is reachable while its descriptor is config-rooted; `ocx patch update` re-point drops the old descriptor + any now-unreferenced old companion digest on `ocx clean`; no `refs/symlinks/` root is ever written for a companion against a base install symlink (C6; the GC-contract blocker Codex flagged).
- [ ] `ocx patch update` re-points a companion `:latest` to a new digest; base install dir + `resolve.json` byte-unchanged; next `ocx exec` reflects new value (C4).
- [ ] Zip warm `OCX_HOME`, unzip elsewhere, `ocx exec --offline` → identical patched env (C6).
- [ ] `ocx clean` collects companion + descriptor when last patched base uninstalled; retains when another patched base still references (C6).
- [ ] Cold + offline + patches configured + never-synced → clear "patch index not synced" error; synced-no-patch → passes through.
- [ ] `OCX_PATCHES` forwarded; grandchild `ocx` (launcher) applies same patches (C5).
- [ ] No regression for unconfigured patches (no `[patches]` → byte-identical behavior).

---

## Links

- Supersedes: `feature/patches:adr_infrastructure_patches.md` (2026-03-22, graph model).
- `adr_two_env_composition.md` — the composition model patches slot into.
- `adr_oci_registry_mirror.md` — mirror interaction.
- `adr_oci_artifact_enrichment.md` — `__ocx.` well-known tag convention.
- `research_infrastructure_patches_overlays.md` — Kustomize / mise overlay analogs (retained).
- Milestone #111; sub-issues #112–#117.

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-22 | mherwig | Initial draft (EdgeKind/Kahn graph model) — on `feature/patches`. |
| 2026-06-19 | architect | Rebase onto two-env composition; reframe patches as site-loaded packages via compose-time overlay (Option B); add independent-update mechanism; settle artifacts-name / descriptor-persistence / mirror / ConstantTracker. Supersedes the draft. |
| 2026-06-19 | architect (post-Codex) | Fix 3 cross-model blockers: (1) overlay is a **two-phase global-last** append, not per-entry interleave (else root vars clobber dep patches, breaking C1); (2) companions projected **interface-only** regardless of target surface (else private-surface leak under `self_view=true`); (3) companions GC-anchored via the **descriptor's `refs/deps/`** rooted by the `artifacts/.../patch` symlink — never a `refs/symlinks/` back-ref to the base install symlink (unimplementable under `ReferenceManager`). |
