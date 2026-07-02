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
**Amendment (2026-06-21):** the global ("root") descriptor moved from the patch-registry **root** (empty repository path) to a reserved single-segment repository named **`global`** (`<patch-registry>/global:__ocx.patch`). An empty repository path is not a valid OCI repository name — path components must start with `[a-z0-9]` — so Docker `registry:2`/Harbor/GHCR reject it (`NAME_INVALID`); the reserved `global` name is portable and structurally cannot collide with any per-package sub-path (the path template always yields ≥2 segments). The `ocx patch publish --global-root` flag is renamed `--global`. References below to "the patch path root" / "root descriptor" mean this reserved `global` repository.
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

The load-bearing requirements (C-numbers used throughout):

| # | Requirement |
|---|---|
| **C1** | **Compositional load.** A site package's *interface* environment composes into the *root execution environment*, overriding the publisher's values for the same key (infra intent wins). |
| **C2** | **Entry-point tiering.** Entry points run in their own composed environment (`self_view=true`) and must inherit the root's patches; entry-point-owned packages may carry their own patches. |
| **C3** | **Visibility tiering.** A patch for a dependency that reaches the root only on the *private* surface must load **only** into the private surface. Patches inherit the interface/private visibility of the target they patch. |
| **C4** | **Site policy floats; identity does not.** Patches are an **ambient site tier** (the execution-env twin of `[mirrors]`), not lockfile content. The active policy refreshes **independently of base-package installs** and works offline once synced. Reproducibility of *tools* (the lock) and of *site policy* are orthogonal domains — patch identity is deliberately **not** folded into base-package identity. (Validated by Nix `impureEnvVars`, Cargo `config.toml` source-replacement, Spack site scope, Lmod `SitePackage.lua`, containerd `hosts.toml` — see Industry Context.) |
| **C5** | **Loading guarantee.** Every applicable patch is *always* loaded when its target loads — including patches for transitive deps, including encapsulated entry-point deps needing the patch on their private surface. No silent skips. |
| **C6** | **CAS persistence + GC.** Patch descriptors and companion packages persist in the blob/layer CAS so the offline contract holds (zip `OCX_HOME` → unzip elsewhere → reinstall → identical patched env), and GC must keep them reachable. |
| **C7** | **Operator enforcement + fail-closed.** A security patch (corp CA, truststore) must be enforceable by the *operator* (not silently suppressible by a user) and must **fail closed** when unavailable — a tool that runs without its CA overlay would do TLS to untrusted endpoints, strictly worse than refusing to launch. (K8s mutating+validating-webhook pattern.) |
| **C8** | **Opt-in determinism.** A consumer may freeze the *whole site tier* (companion digests) into a snapshot for reproducible CI — **without** per-package lock pinning. (Spack inline policy / Nix `flake.lock` narHash pattern.) |

---

## Industry Context & Research

A cross-ecosystem SOTA pass (`research_infrastructure_patches_overlays.md`, 2026-06-19) found **convergent agreement** across six independent systems: each maintains a hard split between *artifact identity* (content hash / version) and *site/host execution policy*, placing the policy at a **config tier, never in per-artifact identity**.

| System | Site-policy layer | Kept out of identity by | Lesson for OCX |
|---|---|---|---|
| **Nix / NixOS** | `nix.conf` `ssl-cert-file`, `impureEnvVars` (`NIX_SSL_CERT_FILE`) | impure env explicitly *not* a derivation input → output hash unchanged when cert rotates | The corp-CA case is a **named, blessed impurity**; float it, don't hash it. |
| **Cargo** | `config.toml` `[source]` replacement, `[net]` proxy/CA | not recorded in `Cargo.lock`; lock portable across mirror/direct | Direct precedent for "site policy orthogonal to lock." |
| **Spack** | `system`/`site` scope `packages.yaml` | not in `spack.lock`; determinism via inline `spack.yaml` snapshot | **Whole-tier snapshot** is the determinism escape hatch, not per-pkg pin. |
| **Lmod (HPC)** | `SitePackage.lua` global hooks (license/env vars) | invisible to `module save/restore` | Site policy is ambient host state, operator-owned. |
| **K8s / Istio** | `MutatingWebhookConfiguration`, sidecar injection | separate cluster object; namespace-label opt-in, `inject:"false"` opt-out | **Fail-closed** security mutations (mutating+validating pair); per-workload opt-out. |
| **containerd** | `hosts.toml` mirror/CA (OCX's own `[mirrors]`) | OCI digest unchanged; only endpoint redirected | Patches are the **execution-env twin** of this existing tier. |

The reframe — *patches = ambient site tier like `[mirrors]`, float-default, whole-tier freeze for determinism, fail-closed for security* — is validated by all six, and reuses a model OCX already ships for transport policy.

---

## Considered Options

The *source* decision (where patch descriptors live) was settled by the original ADR and is **retained**: **client-configured patch registry** (operator-owned OCI registry, `[patches]` config, `__ocx.patch` well-known tag). Options 1/2/4 (referrers API, source-registry tag, publisher metadata) all require push access to registries the operator doesn't control, or publisher cooperation — rejected as before.

What this revision decides is the **composition + persistence model**. Two viable options:

### Option A — Install-time TC injection ("site packages as TC entries")

At install, after building the base package's declared-dependency TC (`pull.rs:431`), resolve its patches and **inject the companion packages into the `with_dependencies` iterator** as additional edges. They flow through the `through_edge`/`merge` visibility algebra, land in `resolve.json`, and the composer needs **zero change**. GC works for free (companions are TC entries → walked like any dep). Visibility tiering (C3) is free (companion inherits the target edge's visibility).

**Fatal flaw vs C4.** `resolve.json` lives inside the base package's **content-addressed install directory** (`packages/{registry}/{repo}/sha256/.../resolve.json`). Baking patches into it means: (1) a patch update requires rewriting a CAS object's contents — violates immutability and the offline/portability digest contract (R4 of the mirror ADR's lineage); or (2) re-installing the base package to regenerate `resolve.json`. Both violate C4 ("don't update the base packages in use"). Also, patch config is *global client config*, not publisher metadata — folding it into a per-install immutable artifact couples two lifecycles that must stay independent.

### Option B — Compose-time site overlay ("site layer") **(RECOMMENDED)**

Keep base-package TCs **pure** (publisher-declared deps only). Maintain a **separate, independently-synced site layer** that is **derived, not cached**: the companion packages are **ordinary installed packages** (in `packages/`, hardlinked from `layers/`, blobs in `blobs/` — no new store), discovery state is the existing **tag store** (`tags/`), and the descriptor is a **persisted blob** (`blobs/`). At compose time the composer, **for each TC entry it already visits** (and for the root), re-derives the matched companions from the persisted descriptor blob + unified match and emits their interface env **gated by the same surface boolean** the TC entry resolved to. No ledger.

- **C1** ✓ companion interface vars appended after the base's own vars → infra overrides publisher by append-order (Constant replaces, Path prepends).
- **C3** ✓ companion env emitted under the *visited TC entry's* `want` (`has_interface()`/`has_private()`); a private-only dep's patch lands only on the private surface, for free.
- **C4** ✓ the site layer is re-synced independently (`ocx patch sync`); base `resolve.json` is never touched; companion packages are their own CAS installs addressed by their own digest.
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
      for companion in site_patches.for(I):         // pre-resolved SitePatchSet, passed into compose
          emit companion's INTERFACE vars only       // never the companion's private surface
```

The overlay needs `[patches]` config + tag/descriptor reads to know which companions apply — which `composer::compose(roots, store, self_view)` does **not** have (it sees only `PackageStore`). So the overlay is **not** derived inside the composer. The `PackageManager`/`Context` layer (where `resolve_env` lives, and which holds the resolved `PatchConfig`) builds a **`SitePatchSet`** via `SitePatchResolver` and passes it in: `compose(roots, store, self_view, &site_patches)` (or, equivalently, Phase 2 runs in `resolve_env` *after* `compose` returns Phase 1's admitted set). Inside compose, `site_patches.for(I)` is a pure lookup into the pre-resolved set — no config read, no I/O. This mirrors how the mirror feature resolves a `MirrorMap` at `Context` (`resolve_mirror_map`) and hands it to the client rather than having the client read config.

Two invariants make this correct:

- **Global-last ordering (C1).** Every companion var is emitted after *all* publisher vars (deps and roots), so infra always overrides publisher for shared keys, regardless of whether the patched package is a deep transitive dep or the root. Deterministic companion order: targets in Phase-1 visit order, then descriptor rule order, then companion-list order.
- **Interface-only projection (no private leak).** A companion is emitted exactly like a dependency — only its `has_interface()` vars cross into the target env (`emit_interface_vars` semantics, `composer.rs:343`). The **target's** surface (`want`) decides *whether* the companion block is emitted; it never decides *which of the companion's surfaces* is read. Even under a launcher's `self_view=true`, the companion contributes only its interface vars — its own private env / private deps stay sealed. (This corrects an earlier "companion as visibility-inherited sub-root" framing, which would have leaked the companion's private surface under `self_view=true`.) A companion's *own* transitive deps contribute through the companion's interface projection, resolved when the companion was installed.

The `SitePatchSet` itself resolves entirely from local state (`[patches]` config + tag store + persisted descriptor blobs → companion installs under `packages/`) — no ledger, no network. It is built once per `resolve_env` call and reused for every root.

**Why "the visited entry's surface gates the block" is the whole of C3.** The composer already computed, per root, whether identifier `I` reaches the interface or private surface (`composer.rs:122`). The patch is *for* `I`, so its block is emitted iff `I` was admitted — a private-only dep's patch is therefore present only under `self_view=true` and absent from the default interface env. No new visibility algebra: the gate reuses the boolean the composer already has; the projection reuses the interface-only dep emission.

### Entry-point inheritance (C2)

Entry-point launchers call `manager.resolve_env(&[pkg], self_view=true)` (`crates/ocx_cli/src/command/launcher/exec.rs:54`) — there is **no separate entry-point environment**; the launcher composes the owning package with the private surface. Therefore:
- Root patches are inherited by entry points automatically: same `compose`, `self_view=true` visits the full TC, overlay applies.
- A dep that is private to the package but patched gets its patch on the private surface inside the entry point (C3 + C5 together).
- "Entry-point-owned patches" = patches whose descriptor targets the entry-point-owning package's identifier; they are in that package's TC, so they overlay like any other.

No per-entry-point patch config is introduced (the launcher model has no per-entry env to attach it to). If future need arises, it is additive.

### The site tier — abstraction, scopes, update, enforcement, freeze (C4/C7/C8)

**Patches are a third configuration tier**, sibling to `[mirrors]`, orthogonal to the two toolchain systems (project `ocx.toml`/`ocx.lock` and global `ocx --global add/remove`) and to package space. They are *not* a dependency, *not* a lockfile entry, *not* added to `ocx.toml`. The right mental model is *host/site policy that adapts whatever runs* — the execution-env twin of the mirror tier (which adapts where bytes come from). This dissolves the earlier per-package-pinning awkwardness and the "`ocx patch update <which package>`" ambiguity: a site tier is refreshed and frozen **as a whole**, at the host level, never per package.

**Immutable identity; state is derived at the manager layer (no ledger).** The descriptor blobs and each companion *package* are immutable content-addressed CAS objects (one digest per version) in the **existing** `blobs/`/`layers/`/`packages/` stores — a companion is just a package; **no new store, and no `state/patches.json` ledger.** The set of patches that apply is a **pure function** of state already on disk: installed bases (`symlinks/`) + `[patches]` config + the tag three-state (`tags/{patch_registry}/{repo}.json`) + descriptor blobs (`blobs/`). It is computed by a **`SitePatchResolver` at the `PackageManager`/`Context` layer — which holds both the resolved `PatchConfig` and the `FileStructure`** — and the resulting **`SitePatchSet`** (per-base + global → companion/descriptor digests) is passed **explicitly** into the two consumers: `composer::compose` (the overlay) and `clean`/GC (extra roots). Leaf `compose`/GC do **not** read config themselves; the command layer that already has `Context` resolves the set and hands it down — the same way `resolve_mirror_map` resolves mirrors at `Context` and hands a `MirrorMap` to the client. The *content* is always digest-verified — what floats is *which policy is active*. The **only** persisted patch-specific artifact is the opt-in freeze snapshot (below). (This corrects an over-clever earlier claim that GC could derive patch roots with *no* config: `collect_project_roots` is config-free because project roots live in the symlink store, but a base→companion mapping needs the patch registry/template from `[patches]`, so the resolver runs where config is available — at the command layer, not inside the GC leaf.)

**Discovery is lazy, not eager (corrects an earlier "load all patches" framing).** Three sources:

- **Package-specific — lazy, on base install** (pull-through, like index tag-resolution). Installing a base package computes its patch repo (path template), looks up `__ocx.patch`, records the outcome in the **tag store** (`tags/{patch_registry}/{repo}.json`): tag present → pull + persist descriptor blobs, install matched companions; tag absent → the tag file exists without an `__ocx.patch` key, i.e. *"looked, no patch"* (not re-checked until a sync). OCX never enumerates "all patches in the registry" — that set is unknowable and mostly irrelevant.
- **Global — a descriptor at the root of the patch namespace** (not a config list): the `__ocx.patch` at the configured patch path's root, checked once per sync, auto-loaded if present, silent if absent. See "Patch scope" below.
- **Re-check — explicit sync** (below).

**Config scopes (C7 enforcement).** `[patches]` is read through OCX's existing tier stack, with one enforcement rule added:

- **System scope** (`/etc/ocx/config.toml`, root-owned) — operator-enforced patches. A `required` patch declared here is **non-overridable**: the normal last-wins merge is replaced by *system-required entries are additive and cannot be removed or redirected by a lower tier*. A user cannot suppress the corporate CA.
- **User scope** (`$OCX_HOME/config.toml`, `--config`, `OCX_CONFIG`) — personal/non-enforced patches and `required = false` overrides.

**Fail posture (C7).** Each patch entry carries `required: bool`, **default `true`**:

- `required = true` + companion unavailable / never-looked offline → **fail closed**: `ocx exec` errors at launch (a tool that runs without its CA overlay would silently do TLS to untrusted endpoints — strictly worse). Mirrors the K8s mutating+validating-webhook pair.
- `required = false` → **fail open**: skip with a warning (license-server URL, metrics endpoint — non-security).

**Update = re-check the known set (C4).** **`ocx patch sync`** (and a piggyback inside `ocx index update`) re-checks the patch repos for the **packages you actually have** + the root global descriptor — not the whole registry. For each installed base repo (and the root), re-resolve the patch repo, fetch `__ocx.patch`, and where the digest advanced: persist the new descriptor blobs (re-pointing the tag file) and install newly-named companion digests. **No per-package `update <pkg>` verb** (the category error) and **no whole-registry crawl** (unknowable); the unit is "refresh what this host uses," like `puppet apply` / module refresh. `sync` is also how patches are discovered for packages installed *before* `[patches]` was configured. Base `resolve.json` and install dirs are **never rewritten** — decoupled from base digest (a JDK pinned at digest X keeps running; re-pointing `zscaler-root:latest` only updates the tag file + pulls the new companion; the next exec re-derives).

**Determinism = whole-tier freeze, mirroring `OCX_INDEX` (C8).** **`ocx patch freeze`** resolves the current site tier and writes the **companion + descriptor digests** into a **site-scoped** snapshot (sibling to `ocx.lock`, e.g. `patches.snapshot.json`) — *not* per-package lock entries, *not* the globally-portable base lock. Selection mirrors the existing index-snapshot mechanism (`OCX_INDEX`, `env.rs:47`, forwarded only when explicitly set, `env.rs:279`): an analogous `OCX_PATCH_SNAPSHOT`-style selector points at a frozen snapshot → compose **prefers the snapshot's pinned digests**; absent → compose re-derives from live tags (floating). The snapshot is **policy, not truth**: it can go stale vs live tags and is advanced only by `ocx patch sync` / re-`freeze`; compose precedence is *snapshot-under-selector wins, else live*. Two portability domains stay clean: the base lock is globally portable (public, content-addressed); the patch snapshot is **site-scoped** (valid only inside the org that owns the patch registry). This is the Spack-inline-policy / Nix-`flake.lock` whole-graph-snapshot pattern.

**Per-package opt-out.** A package that manages its own trust stack can decline patches via `[package."<id>"] no-patches = true` in `ocx.toml` (Istio `inject:"false"` analog). Orthogonal to the descriptor `match`, which already scopes *which* base packages a patch targets.

**Offline at exec.** Once synced (or frozen), compose reads only local state (tag store + descriptor blobs + companions). Warm + offline → full success. Cold + offline + a `required` patch never-looked → fail-closed error (three-state discovery below). Floating, not auto-updating: like the index, the site tier never refreshes itself behind your back — `ocx patch sync` is always explicit, honoring the offline-first / never-auto-update-index principle.

### Settled open questions

1. **No new store, no ledger — companions are packages; state is derived.** The earlier `artifacts/` symlink store **and** the proposed `state/patches.json` ledger are both **dropped**. A companion lives in `packages/` (hardlinked from `layers/`, blobs in `blobs/`) like any package — a patch is not a new *kind* of content. Discovery state lives in the existing **tag store** (`tags/{patch_registry}/{repo}.json`, three states); descriptor blobs in `blobs/`. Everything else (which companions apply, the GC roots, the compose overlay) is **re-derived** from those + `[patches]` config + installed bases. The only persisted patch-specific artifact is the **opt-in freeze snapshot**. No 8th store, no ledger to keep coherent. (Follow-up: `subsystem-file-structure.md` documents only 6 stores and omits `state` — stale; correct it independently.)
2. **Descriptor persistence (light) + how GC finds companions.** `pull_description` (`oci/client.rs:1030`) reads layer blobs into memory and never persists them. For patches, persist the descriptor's blobs via `BlobStore::write_blob` (so they survive offline + are auditable). The descriptor is **not** assembled into a full installed package — nothing execs it. **How GC enumerates a base's companions:** not by walking the base's refs (a descriptor blob has no outgoing companion ref, and tag files are not CAS — so a "reachable-from-base-refs" invariant is *not* achievable; an earlier draft wrongly asserted it). Instead the `SitePatchResolver` recomputes the mapping at the command layer from `[patches]` config + tag file + descriptor blob (next section). The companions are full packages; the resolver yields their digests, which `clean` seeds as roots.
3. **Mirror interaction.** The patch registry is treated as any other registry: **subject to `[mirrors]` rewriting** (no special exemption). In practice the patch registry is an internal host rarely present in the mirror map, but exempting it would be a surprising special case; consistency wins. The operator owns both `[patches]` and `[mirrors]`.
4. **`ConstantTracker` role.** Under two-env compose, override resolution is **append-order last-wins** (`Modifier::Constant` replaces, `Modifier::Path` prepends) — the overlay appends companion vars after the base, so "patch overrides publisher" is intentional and automatic, needing **no** `EdgeKind`-aware tracker. Per-var info/warn conflict reporting is demoted to a **display concern** surfaced by `ocx env --show-patches` annotations, not a composition gate. This deletes the bulk of the old Phase 4. (`ConstantTracker` at `package/metadata/env/conflict.rs` stays as-is for the CI flavors that already use it.)

### Config forwarding (C5 across process boundaries)

Add `patches: Option<PatchConfig>` (resolved form) to `OcxConfigView` (`env.rs:72`) and an `OCX_PATCHES` key in `apply_ocx_config` (`env.rs:248`), forwarded as JSON like `OCX_MIRRORS`. Generated launchers re-enter `ocx launcher exec`, which must resolve the **same** patch layer or an entry point would silently lose patches — so forwarding is mandatory, decided in Phase 1 even though consumed in Phase 3+.

### Consequences

**Positive:** reuses the two-env composer (no graph machinery), the `[mirrors]` site-tier precedent, the existing CAS (companions are just packages — **no new store, no ledger**), and the GC's existing re-derive-roots-from-`FileStructure` pattern (`collect_project_roots`); visibility tiering is free; the site tier decouples cleanly from base installs and the project/global toolchains; old Phases 3–5 shrink (no `EdgeKind`, no Kahn, no `ConstantTracker` rework).
**Negative:** a genuinely new site-tier surface (`ocx patch sync` / `ocx patch freeze` / `ocx patch publish` / `ocx patch test`, scopes, `required` fail-posture, lazy three-state discovery) the old ADR hand-waved; a new GC root re-derivation to wire (no persisted pin list, but a per-base derive step); descriptor blob-persistence is new code (closes the `pull_description` gap); the unified flat matcher; composer gains an overlay pass touching a hot path.
**Risks:**

| Risk | Mitigation |
|---|---|
| `required` patch cold + offline + never-looked | Three-state discovery → fail-closed error (C7). `required=false` patches warn + skip. |
| Companion tag drift (`:latest` re-pointed) | `ocx patch sync` refreshes; `ocx patch freeze` pins digests for determinism (C8). |
| User suppresses an enforced corp-CA patch | System-scope `required` entries are additive + non-overridable by lower tiers (C7). |
| Composer hot-path cost of per-entry overlay lookup | `SitePatchSet` built once per `resolve_env` at the manager layer; in-compose lookup is a pure map probe (no I/O). |
| GC over-collects companions | `clean` (which has `Context`/config) builds the `SitePatchSet` via `SitePatchResolver` and passes companion digests as an extra root set to `ReachabilityGraph::build` — companions of installed bases survive (see GC section). |
| Forgotten forwarding → entry points lose patches | `OCX_PATCHES` forwarding is a Phase-1 acceptance criterion + a Block-tier review item (mirrors the `OCX_MIRRORS` rule). |

---

## Technical Details

### Configuration (`[patches]`)

```toml
# System scope (/etc/ocx/config.toml) — operator-enforced. required=true entries
# are additive + non-overridable by lower tiers.
[patches]
registry = "internal.company.com/ocx-patches"
path = "{registry}/{repository}"   # default; {registry} slugified via to_relaxed_slug()
required = true                    # default; fail-closed if a matched companion is unavailable
# ocx.sh/java:21 → internal.company.com/ocx-patches/ocx_sh/java:__ocx.patch

# A package that manages its own trust stack opts out (in project ocx.toml):
[package."ocx.sh/some-tool"]
no-patches = true
```

`PatchConfig { registry: Option<String>, path: Option<String>, required: Option<bool> }` mirrors `config/mirror.rs:26`; `resolve_patch_config(&config, env)` mirrors `resolve_mirror_map`; both re-exported from `lib.rs`. `required` defaults to `true`. There is **no** `global = [...]` field — global patches are a descriptor at the patch path's root (see "Patch scope"). Tier merge is last-wins **except** that a system-scope `required` entry cannot be removed or redirected by a lower tier (enforcement, C7). `[patches]` env selection (registry, snapshot) mirrors `OCX_INDEX` so dev vs CI is chosen by **which config/registry the host points at** — not a schema field on the descriptor. The placeholder tripwire test `parse_unknown_future_patches_section_silently_ignored` (`config.rs`) flips to positive parse tests.

### Patch descriptor (`__ocx.patch`)

OCI ImageManifest, `artifactType: application/vnd.sh.ocx.patch.v1`, single layer `application/vnd.sh.ocx.patch.descriptor.v1+json`:

```json
{ "version": 1, "rules": [
  { "match": "*",                "packages": ["internal.company.com/certs/zscaler-root:latest"] },
  { "match": "ocx.sh/java:*",    "packages": ["internal.company.com/java-config/jdk21-truststore:1.0"] }
] }
```

`PatchDescriptor { version: u32, rules: Vec<PatchRule> }`, `PatchRule { match_pattern: String, packages: Vec<Identifier>, required: Option<bool> }`. Per-rule `required` overrides the tier default. Companion packages are ordinary OCX packages (env-only or content-bearing, e.g. a CA bundle exposing `SSL_CERT_FILE`). Matches accumulate (union, dedup by identifier, first wins).

#### One match semantic — no global/package-specific drift

`match` is a glob over the **full canonical identifier string** `registry/repository:tag[@digest]` (the `Identifier` Display form, `oci/identifier.rs:264`), used **identically** for the root global descriptor and every package-specific sub-path descriptor. There is **no** path-glob-for-global vs tag-glob-for-package split — that was the drift to avoid.

The engine is a **flat matcher** (not `glob::Pattern`, which is path-aware and stops `*` at `/`): `*` spans any run of characters *including* `/`, `:`, `@`; `?` = one char; `[set]` = class; everything else literal; case-sensitive; first-match-wins. ~20 lines, no `**`, no new dep.

| `match` | matches | does not |
|---|---|---|
| `*` | everything | — |
| `ghcr.io/*` | `ghcr.io/acme/cli:v1` | `ocx.sh/java:21` |
| `*/java:*` | `ocx.sh/java:21`, `ghcr.io/java:latest` | `ghcr.io/cmake:3.28` |
| `*:21` | `ocx.sh/java:21`, `ghcr.io/jdk:21` | `ocx.sh/java:3.28` |
| `*zscaler*` | `internal.company.com/certs/zscaler-root:latest` | `ocx.sh/java:21` |

Edge cases (state in the descriptor-author docs): a registry **port** (`localhost:5000/...`) and a **digest** (`@sha256:…`) contain `:` — the flat matcher treats `:` as a literal, so they are safe (no field parsing). An **untagged** identifier (`ghcr.io/cmake`, no `:`) will *not* match `*:*` by design — explicitness over silent match.

### Patch scope: global vs package-specific — one mechanism, scope = location

Both kinds are the **same artifact** (a `__ocx.patch` descriptor) and the **same match** (§ above); they differ only in **where the descriptor lives** and **when it is fetched** — there is no separate "global" config concept and no magic `__global__` token:

| Scope | Descriptor location | Fetched | Example |
|---|---|---|---|
| **Global / site** | `__ocx.patch` at the **root** of the configured patch path (`<patch-registry>:__ocx.patch`) | once per `sync`; **auto-load-if-exists, silent if absent** | every tool gets the corp CA bundle |
| **Package-specific** | `__ocx.patch` at a **sub-path** via the template (`<patch-registry>/<template(base)>:__ocx.patch`) | **lazily, on that base's install** | Java gets the JDK truststore |

The path template always emits a non-empty sub-path, so the root can never collide with a package-specific repo — the root is reserved for the global descriptor by construction. A **global patch is a descriptor**, so it naturally carries **multiple companion packages** (multiple `rules`, multiple `packages`) — no separate "always-load list" needed. At compose, the global descriptor's matched companions overlay every root's interface surface (Phase 2, **before** package-specific overlays so the latter can still override). One artifact type, one match engine, one overlay path; scope is just discovery location.

### Storage — existing CAS only (no new store, no ledger)

```
$OCX_HOME/
  packages/.../zscaler-root/...          # companion = ordinary package (hardlinked from layers/)
  layers/...  blobs/...                   # companion + descriptor blobs — existing CAS, deduped
  tags/{patch_registry}/{repo}.json      # three-state discovery (existing TagStore): looked? has __ocx.patch?
  patches.snapshot.json                  # ONLY persisted patch artifact — opt-in `ocx patch freeze` (sibling to ocx.lock)
```

Nothing else is persisted. Which companions apply to a base, the GC roots, and the compose overlay are all **re-derived** from `tags/` + descriptor blobs + `[patches]` config + installed bases. Companions are **not** placed under `symlinks/` (not user-selected) unless an operator also runs `ocx install <companion>` (an orthogonal, independent root).

### GC reachability (C6)

Reuses the BFS in `reachability_graph.rs` with **one new root set, computed by the `clean` command (which holds `Context`/config) and passed into the build — no persisted pin list, no config inside the GC leaf.** Today `ReachabilityGraph::build(file_structure, project_roots)` takes a pre-computed `project_roots` set (`reachability_graph.rs:71`; `clean.rs:214` `collect_project_roots`). The change: `clean` *also* builds a `SitePatchSet` via `SitePatchResolver` (config + FileStructure) and passes its companion + descriptor digests as an **additional root set** — `build(file_structure, project_roots, patch_roots)`. This **supersedes** both the `artifacts/`-symlink anchoring and the proposed ledger, and avoids the two dead ends: a `refs/symlinks/` back-ref against the base install symlink is unimplementable (`ReferenceManager::link()` owns both sides and would repoint the base install, `reference_manager.rs:64`), and mutating the base's content-addressed `resolve.json` is barred (C4). Deriving the set at the command layer needs neither.

- **Roots derived per installed base, at the command layer.** `SitePatchResolver` enumerates installed bases (symlink store), and for each computes its patch repo from `[patches]` (registry + template), reads the local tag file + persisted descriptor blob, applies the unified match → companion + descriptor digests. The **root global descriptor** is resolved once. `clean` seeds all of these as roots; the normal BFS then walks each companion's `refs/deps`/`refs/layers`/`refs/blobs`. Offline-safe: config is local, tags + descriptor blobs are local CAS reads — no network. (Tag files are plain metadata, never GC'd; descriptor blobs survive because their digest is a seeded root.)
- **Package-specific lifetime = tied to base install (chosen).** Uninstall the last base that resolves a companion → the resolver no longer yields it → on `ocx clean` it (and its descriptor, if not rooted by another live base, the global descriptor, or an explicit `ocx install`) is collected with its layers/blobs. Uninstalling the last patched JDK collects its truststore companion — dependency-like.
- **Global lifetime = tied to config.** The root descriptor's companions are roots while `[patches]` resolves the path and the root `__ocx.patch` exists. Remove `[patches]` → the resolver yields nothing for the root → collected on next `ocx clean`.
- **`ocx patch sync` and old digests.** Sync re-points tag files + pulls new descriptor/companion blobs; superseded digests stop being resolved → fall out on the next `ocx clean`. No back-ref juggling, no base-install mutation, no ledger to drift.

### Three-state discovery (C4/C5 offline contract)

Discovery is **lazy, recorded on base install in the existing TagStore** `tags/{patch_registry}/{repo}.json` — no bespoke ledger. Presence + the `__ocx.patch` key encode all three states, disambiguating the two meanings of "missing patch":

| State | TagStore | Meaning | Behavior |
|---|---|---|---|
| **Never looked** | tags file absent | patch repo never checked (offline before first sync, or brand-new package) | Look it up if online. Offline + a `required` patch → **fail closed**; `required = false` → warn + skip. |
| **Looked, no patch** | tags file present, `__ocx.patch` key absent | *"We simply don't patch this package."* — normal, valid | **Skip silently.** Not re-checked until a sync. |
| **Looked, has patch** | tags file present, `__ocx.patch` → digest | Patch applies | Re-derive + compose companions from the persisted descriptor blob. |

"Looked, no patch" vs "never looked" is exactly *"this package isn't patched"* vs *"we don't yet know."* The fail-closed vs skip decision keys off the patch's `required` flag (C7). `ChainMode::Offline` (`oci/index.rs`) already blocks unpinned-tag resolves with `PolicyBlocked` (exit 81); the never-looked `required` case maps to the same contract. Zipping a warm `OCX_HOME` carries companions (`packages/`) + descriptor blobs (`blobs/`) + tag state (`tags/`) → fully self-contained offline; nothing to derive needs network.

### Patch maintainer workflow — authoring, testing, publishing

A patch maintainer authors a descriptor + companion packages, **tests** the composition locally, then **publishes** to the operator's patch registry. Both new commands reuse existing pipelines; almost nothing is net-new machinery.

**`ocx patch publish`** — descriptor + companions to the patch registry.
```
ocx patch publish <patch-repo|--root> --descriptor patch.json \
  [--companion <pkg> <archive> ...] [--cascade]
```
- **Reused:** companion packages push via `Publisher::push` + cascade (`publisher.rs:24-137`, push `:74`, cascade `:84`). The descriptor manifest is a near-copy of the `__ocx.desc` publish path — `Client::push_description` (`oci/client.rs:943-1024`; blob push `:960`, manifest push `:1017`) + `ManifestBuilder::artifact_type` (`oci/manifest_builder.rs`). Layer-ref CLI parsing precedent at `command/package_push.rs:62`.
- **New (small):** the descriptor JSON schema/parser; build the manifest with `artifact_type("application/vnd.sh.ocx.patch.v1")` + single layer `…descriptor.v1+json` + empty `{}` config; push to the **root** `__ocx.patch` tag (global) or the template sub-path tag (package-specific). No new OCI machinery.

**`ocx patch test`** — dry-run compose onto a base, locally, no publish.
```
ocx patch test <base-id> --descriptor patch.json \
  [--script test.star | -- <cmd>...]
```
- **Reused (~90%):** the `ocx package test` materialize→compose→exec pipeline (`command/package_test.rs:98-202`) + the Starlark host API with `expect.*` assertions (`script/expect_module.rs`, `ScriptOutcome` `script.rs:64`) + Phase-1 composer. A maintainer asserts the resulting env keys/values, or just inspects via `ocx env --show-patches`.
- **New (small):** load the descriptor from a local file and inject its matched companions into the Phase-2 overlay (the same overlay Phase 4 adds) **instead of** deriving from live tags — i.e. a local-descriptor override seam for pre-publish testing (analogous to how `OCX_MIRRORS`/`OCX_INDEX` override resolution, `env.rs:248`).

This closes the authoring loop: write descriptor → `ocx patch test` (assert env) → `ocx patch publish` → operators `ocx patch sync`.

---

## Implementation Plan (re-derived under Option B)

Mapping to issues #112–#117. **Shrinks** vs the old graph plan are marked ↓; **new** work from C4 is marked ✚.

### Phase 1 — `[patches]` config + forwarding (#112)
- `PatchConfig` + `resolve_patch_config` (mirror `config/mirror.rs`), `Config` field + merge, `lib.rs` re-export, path-template expansion (`to_relaxed_slug`), `Context` wiring.
- ✚ `OCX_PATCHES` in `OcxConfigView` + `apply_ocx_config` (forwarding decided now).
- Flip the placeholder tripwire test to positive parse/expansion tests.
- *Validity under reframe: unchanged and confirmed.* Low risk.

### Phase 2 — Descriptor types + unified match + persisted `__ocx.patch` pull (#113)
- `PatchDescriptor`/`PatchRule` serde types.
- ✚ **Unified flat matcher** `glob_match(pattern, identifier)` over the canonical `Identifier` Display string — `*` spans `/`/`:`/`@`, no `**`, **not** `glob::Pattern` (path-aware). ~20 lines + table tests (the `*`, `ghcr.io/*`, `*/java:*`, `*:21` cases + port/digest/untagged edges).
- ✚ **Persist** descriptor blobs (`BlobStore::write_blob`) — close the `pull_description` in-memory gap (blobs only, no package assembly). **No ledger, no new store** — companions use existing CAS; discovery state is the tag file.
- `__ocx.patch` auto-hidden already (no work).

### Phase 3 — Lazy discovery + companion install (#114) ↓
- `SitePatchResolver`: on base install, base identifier → patch repo (template) → `__ocx.patch` (sub-path) + always the **root** `__ocx.patch` (global); persist descriptor blobs; apply the unified match; install matched companions as regular packages.
- Three-state is the **tag file** (present/absent + `__ocx.patch` key). No ledger written.
- **No `EdgeKind`, no Kahn, no graph, no `artifacts/` store, no ledger.** Large shrink vs old Phase 3.

### Phase 4 — `SitePatchResolver` + compose-time overlay + display (#115) ↓
- ✚ `SitePatchResolver` at the `PackageManager`/`Context` layer → `SitePatchSet` (from `[patches]` config + tag store + descriptor blobs). `resolve_env` builds it and threads it into the overlay.
- Add the **Phase-2 overlay**: extend `compose(roots, store, self_view, &site_patches)` (or run Phase 2 in `resolve_env` after `compose` returns the admitted set). Phase 1 records the admitted-identifier set per active surface; Phase 2 appends, globally last, each matched companion's **interface-only** vars (gate = target admitted; projection = interface). `site_patches.for(I)` is a pure lookup — **no config/I-O inside `compose`**. Global (root) descriptor overlays every root's interface surface, before package-specific. Covers root + entry-point (`self_view=true`) paths.
- `ocx env`/`ocx deps` `[patch]` annotations + `--show-patches`.
- **`ConstantTracker` rework dropped** (append-order last-wins). Large shrink.

### Phase 5 — Site tier: sync + freeze + scopes + fail posture + GC re-derivation (#116) ✚↓
- ✚ **`ocx patch sync`** — re-check the **known set** (installed base repos + root global), re-point tag files + pull new descriptor/companion blobs; + piggyback on `ocx index update`. No per-package `update <pkg>` verb, no whole-registry crawl.
- ✚ **`ocx patch freeze`** → site-scoped `patches.snapshot.json` of companion+descriptor digests; selection mirrors `OCX_INDEX` (`OCX_PATCH_SNAPSHOT`-style) — compose prefers snapshot under the selector, else live. Optionally project-scoped.
- ✚ Config **scopes** (system `/etc/ocx/config.toml` enforced vs user, non-overridable `required`) + **`required`** fail posture (default true → fail-closed; false → warn/skip) + per-package `no-patches` opt-out.
- ✚ **Patch GC roots from the command layer.** `clean` builds the `SitePatchSet` (config + FileStructure) and passes companion+descriptor digests as an extra root set: `ReachabilityGraph::build(file_structure, project_roots, patch_roots)` (extends `clean.rs:214` / `reachability_graph.rs:71`). **No persisted pin list, no base mutation, no config inside the GC leaf** — the command resolves roots, the leaf consumes them (same shape as `project_roots`).
- Three-state handling (never-looked offline + `required` → error; looked-no-patch → skip).

### Phase 6 — Acceptance tests + docs + maintainer commands (#117)
- ✚ **`ocx patch publish`** + **`ocx patch test`** (reuse `Publisher::push` / `push_description` / `package test` + Starlark `expect`; see "Patch maintainer workflow").
- `test/tests/test_patches.py`: compose order + override (incl. global-last over root); unified-match table incl. port/digest/untagged edges; global (root) vs package-specific (sub-path); visibility tiering (private-only dep patch absent on interface, present under `--self`); entry-point inheritance; `ocx patch sync` (re-point `:latest`, no base reinstall); `ocx patch freeze` + `OCX_PATCH_SNAPSHOT` pin/float; `required` fail-closed vs `required=false` skip vs looked-no-patch skip; system-scope enforcement non-overridable; offline zip/unzip parity; GC collect/retain (derive-only, no over-collect); `ocx patch test` dry-run asserts composed env.
- Docs: user-guide "Patching packages for your infrastructure"; `[patches]` (scopes, `required`) + `ocx patch sync`/`freeze`/`publish`/`test` reference; cross-link env-composition + `[mirrors]` (sibling tier); schema regen.

---

## Validation

- [ ] Configure patch registry, install base, `ocx exec` shows companion-overridden env in correct order (C1).
- [ ] **Global-last ordering:** a patch on a *transitive dependency* overrides a `Constant` **and** a `Path` var that the **root** package itself declares — patch wins (C1; the Phase-2 regression Codex flagged).
- [ ] **Interface-only projection:** a companion carrying a private-only env var (and a private-only dep) never appears in the patched target's env, even when the target is reached under `--self`/launcher `self_view=true` (no private-surface leak).
- [ ] Private-only dep's patch absent from default (interface) env, present under `--self` (C3); entry-point launcher inherits root patches (C2/C5).
- [ ] **No new store, no ledger:** companion lands in `packages/` (deduped via `layers/`), descriptor blobs in `blobs/`, discovery state in `tags/` — no `artifacts/` dir and no `state/patches.json` created; only `ocx patch freeze` writes `patches.snapshot.json`.
- [ ] **GC roots from `SitePatchResolver` (no pin list):** `clean` derives companion roots from config + tags + descriptor blobs and passes them to `ReachabilityGraph::build`; no `refs/symlinks/` root written against a base install symlink; base `resolve.json` never mutated (C6).
- [ ] **No over-collection (the Codex-flagged break):** install a patched base, run `ocx clean`, then compose **offline** — the companion is **retained** and the patched env reproduces; only when *no* installed base (nor the root descriptor) resolves a companion is it collected.
- [ ] **Unified match — no drift:** the same `match` glob behaves identically in the root (global) descriptor and a sub-path (package-specific) descriptor; table cases pass incl. `ghcr.io/*` crossing `/`, `*:21`, port `localhost:5000/x:1` (`:` literal), digest `@sha256:` (`:` literal), untagged not matching `*:*`.
- [ ] **Global = root descriptor:** `__ocx.patch` at the patch path root overlays every exec and carries multiple companions; a sub-path descriptor overlays only the matching base; package-specific overrides global on a shared key.
- [ ] **Site sync (C4):** `ocx patch sync` re-points a companion `:latest` to a new digest; base install dir + `resolve.json` byte-unchanged; next `ocx exec` re-derives the new value.
- [ ] **Freeze (C8):** `ocx patch freeze` pins digests into `patches.snapshot.json`; under the `OCX_PATCH_SNAPSHOT` selector a later `ocx patch sync` upstream change does **not** alter exec env until re-frozen; without the selector, exec floats; base lock byte-unchanged.
- [ ] **Maintainer loop:** `ocx patch test <base> --descriptor` composes a local (unpublished) descriptor onto a base and a Starlark `expect` asserts the env; `ocx patch publish` round-trips a descriptor + companions to a registry and a client `sync` picks them up.
- [ ] **Enforcement (C7):** system-scope `required` patch cannot be suppressed by a user-scope override; `required` unavailable → fail-closed; `required=false` unavailable → warn + skip.
- [ ] Zip warm `OCX_HOME`, unzip elsewhere, `ocx exec --offline` → identical patched env (C6).
- [ ] **Lazy discovery:** patch resolved on base install (not by enumerating the registry); a base with no patch records "looked, no patch" and is not re-checked until `ocx patch sync`.
- [ ] **Missing-patch disambiguation:** "looked, no patch" passes through silently; "never looked" + `required` + offline → fail-closed — distinct states.
- [ ] `OCX_PATCHES` forwarded; grandchild `ocx` (launcher) applies same patches (C5).
- [ ] No regression for unconfigured patches (no `[patches]` → byte-identical behavior).

---

## Links

- Supersedes: `feature/patches:adr_infrastructure_patches.md` (2026-03-22, graph model).
- `adr_two_env_composition.md` — the composition model patches slot into.
- `adr_oci_registry_mirror.md` — mirror interaction.
- `adr_oci_artifact_enrichment.md` — `__ocx.` well-known tag convention.
- `research_infrastructure_patches_overlays.md` — Kustomize / mise overlay analogs (original) + **2026-06-19 SOTA pass**: Nix `impureEnvVars`, Cargo `config.toml`, Spack scopes, Lmod `SitePackage.lua`, K8s/Istio, containerd `hosts.toml`.
- Milestone #111; sub-issues #112–#117.

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-22 | mherwig | Initial draft (EdgeKind/Kahn graph model) — on `feature/patches`. |
| 2026-06-19 | architect | Rebase onto two-env composition; reframe patches as site-loaded packages via compose-time overlay (Option B); add independent-update mechanism; settle artifacts-name / descriptor-persistence / mirror / ConstantTracker. Supersedes the draft. |
| 2026-06-19 | architect (post-Codex) | Fix 3 cross-model blockers: (1) overlay is a **two-phase global-last** append, not per-entry interleave (else root vars clobber dep patches, breaking C1); (2) companions projected **interface-only** regardless of target surface (else private-surface leak under `self_view=true`); (3) companions GC-anchored via the **descriptor's `refs/deps/`** rooted by the `artifacts/.../patch` symlink — never a `refs/symlinks/` back-ref to the base install symlink (unimplementable under `ReferenceManager`). |
| 2026-06-19 | architect (post-SOTA) | Add the **site-tier model** (C4 reframed; C7 enforcement+fail-closed; C8 opt-in determinism), validated by a 6-ecosystem SOTA pass. `ocx patch update <pkg>` → host-level **`ocx patch sync`** (whole tier); **`ocx patch freeze`** = whole-tier digest snapshot (site/optionally project-scoped), not per-package lock pins; system vs user config **scopes** with non-overridable enforced `required`; **`required`** fail posture (default fail-closed); per-package `no-patches` opt-out; **global vs package-specific** patches (`global = [...]` list vs `__ocx.patch` descriptor rules); missing-patch disambiguation (no-patch-for-pkg vs tier-never-synced). |
| 2026-06-19 | architect (storage/discovery rework) | **Drop the `artifacts/` store** — a companion *is* a package (`packages/`/`layers/`/`blobs/`); descriptor = persisted blobs only. (Superseded below: the interim `state/patches.json` ledger is also dropped.) **Discovery is lazy** (resolved on base install, three-state in the tag store), not a whole-registry crawl; `ocx patch sync` re-checks the **known set**. **Companion lifetime tied to base install** (global tied to config). |
| 2026-06-19 | architect (grounded rework, post-workflow) | **Drop the ledger too — derive, don't cache.** State = tag store (`tags/`) + descriptor blobs (`blobs/`); only persisted patch artifact is the opt-in freeze snapshot. **Unified match**: one flat glob over the full canonical identifier for global *and* package-specific (not `glob::Pattern`); kills the global/package match drift. **Global = the `__ocx.patch` at the patch path root** (auto-load-if-exists, silent if absent) — no `__global__` token, no `global=[]` config list; it is a descriptor → multi-companion for free. **Pin/float mirrors `OCX_INDEX`** (`OCX_PATCH_SNAPSHOT` selector). **dev/CI via host config**, not a schema field. Added the **patch-maintainer workflow** (`ocx patch publish` / `ocx patch test`) reusing `Publisher::push` / `push_description` / `package test` + Starlark `expect`. |
| 2026-06-19 | architect (post-Codex #2) | Fix the critical "derive + no-config-in-GC + no-base-mutation" inconsistency Codex flagged: that triple is unsatisfiable (GC has no `PatchConfig`; tag files aren't CAS; blobs have no companion ref). Resolution: a **`SitePatchResolver` at the `PackageManager`/`Context` layer** (which holds resolved `PatchConfig` + `FileStructure`) builds a **`SitePatchSet`** that is passed **explicitly** into both consumers — `compose(roots, store, self_view, &site_patches)` and `clean` → `ReachabilityGraph::build(file_structure, project_roots, patch_roots)`. Keeps "no ledger / no base mutation"; corrects the false "no config" claim (config is read at the command layer, like `resolve_mirror_map`→`MirrorMap`, not inside leaf compose/GC). Dropped the unachievable "descriptor+tag CAS-reachable from base refs" invariant. |
