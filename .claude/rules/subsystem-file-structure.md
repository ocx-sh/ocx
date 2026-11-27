---
paths:
  - crates/ocx_store/src/**
  - crates/ocx_util/src/fs/symlink.rs
---

# File Structure Subsystem

## Design Rationale

Old `objects/` tree mix three concerns: raw OCI blobs, extracted layer files, assembled packages. Mix break cross-repo dedup (repo in CAS path), block shared layers (one layer per package), no spot for non-extracted blobs (signatures, SBOMs). Three-tier split fix each problem, clear lifecycle per tier:

- **`blobs/`** — raw OCI content (manifests, compressed layer archives); key by registry + digest; never extracted
- **`layers/`** — extracted layer trees shared across packages; key by registry + digest; dedup unit for multi-layer packages
- **`packages/`** — assembled packages; key by registry + digest only (no repo in path → cross-repo dedup); hardlinked content from one or more layers

Split `refs/` into four named subdirs (`symlinks/`, `deps/`, `layers/`, `blobs/`) → GC do single BFS pass over all three tiers. Only packages can be roots or have outgoing edges; layers and blobs reachable only via package `refs/layers/` and `refs/blobs/` links.

**`TagStore` deleted; `FileStructure` gains `index: IndexStore`.** The
tag-only local index is replaced by the wire-grammar index collection (dispatch-object-only CAS —
see `subsystem-oci.md` "LocalIndex — wire-grammar collection, dispatch-only"). `IndexStore` is
`FileStructure`'s default machine-local index home (`root.join("index")`) — never one of the
GC-walked stores below. `LocalIndex` (the `oci::index` consumer) reads through it *by default*;
`--index` PATH / `OCX_INDEX` construct a separate `IndexStore` pointed elsewhere instead
(`context.rs`), bypassing `FileStructure` for that one override — home resolution stays
`--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index`. ADR: `adr_index_indirection.md` Decision A, B1, H.

Fourth tier `state/` for **ephemeral, non-content-addressed runtime state** (TTL-bound, per-subsystem/per-registry, not GC-walked). Distinct from the three CAS tiers because it carries no digest, has no `refs/` linkage, and may be deleted at any time without integrity loss. See `adr_oci_referrers_signing_v1.md` Amendment 3 for the definitional contract and the originating decision.

## Module Map

| File | Purpose | Key Types |
|------|---------|-----------|
| `file_structure.rs` | Composite root; `slugify()`, `repository_path()` | `FileStructure` |
| `blob_store.rs` | Raw OCI blob storage; stateless `write_blob` / `read_blob` (tempfile + atomic rename + Windows-cfg retry-with-backoff) | `BlobStore`, `BlobDir` |
| `layer_store.rs` | Extracted layer storage | `LayerStore`, `LayerDir` |
| `package_store.rs` | Assembled package storage | `PackageStore`, `PackageDir` |
| `symlink_store.rs` | Install symlinks (candidate/current) | `SymlinkStore`, `SymlinkKind` |
| `temp_store.rs` | Temp dirs for in-progress downloads | `TempStore`, `TempDir`, `TempAcquireResult`, `StaleEntry`, `TempEntry` |
| `state_store.rs` | Persistent + ephemeral runtime state per subsystem (update-check throttle, managed-config, OCI Referrers capability cache, offline-verify trust-root cache) — named per-subsystem accessors, no generic `StateKey`; also the toolchain **render stamps** (C-003) | `StateStore`, `RenderStamp`, `RenderStampScope`, `RenderStampTarget`, `BinEntryStamp` |
| `cas_path.rs` | `CasTier` enum, digest-file IO, shard validation (sharding itself is `ocx_oci::cas_path::cas_shard_path`) | `is_valid_cas_path()`, `write_digest_file()` |
| `ocx_index/src/local_index/store.rs` | Local index collection — wire-grammar store (root docs + dispatch-object CAS, A2); config blobs route through the machine-global `BlobStore` instead | `IndexStore`, `CatalogEntryStatus`, `CatalogTransaction`, `RootReadResult` |
| `shim_store.rs` | Generated launcher directories — the on-disk form of a **deferred** tool (`deferred/<G>/…`; older releases' `shims/…` is only read, by GC) | `ShimStore`, `ShimDir` |
| `../launcher_generation.rs` | The launcher generation key `G` (blob + record schema + revision) and the toolchain render fingerprint | `LauncherGeneration`, `render_fingerprint`, `HOME_LAUNCHER_REVISION` |
| `toolchain_store.rs` | Rendered toolchain home — one grammar, two tiers: the store wraps the **global** home, the home is the value type a project tier reuses | `ToolchainStore`, `ToolchainHome`, `ToolchainPathComponent`, `ToolchainPathError` |
| `reference_manager.rs` | Forward symlinks + back-references for GC | `ReferenceManager` |

### Cross-cutting link primitives

These modules sit outside `file_structure/` — consumed across subsystems.

| Module | Purpose | Used by |
|--------|---------|---------|
| `ocx_util::fs::symlink` | Symlink create/update/remove/is_link; Windows junction aware | ReferenceManager, pull, assemble walker, archive extractor |
| `ocx_util::fs::publish` | `publish_dir` (one rename of a staged directory; `New` or `AlreadyThere`), `retarget_link` (switch a link or Windows junction in place), `retire_dir` (rename aside, then remove; only `ocx clean` calls it) | launcher writers, toolchain render, `ocx clean` |
| `hardlink.rs` | Hardlink create/update — THE ONE place `std::fs::hard_link` lives | assemble walker, codesign |
| `ocx_util::fs::path` | Lexical path helpers: `lexical_normalize`, `escapes_root`, `validate_symlinks_in_dir` | symlink, archive extractor, assemble walker |

## FileStructure (composite root)

```rust
pub struct FileStructure {
    pub blobs: BlobStore,
    pub layers: LayerStore,
    pub packages: PackageStore,
    pub index: IndexStore,
    pub symlinks: SymlinkStore,
    pub state: StateStore,   // persistent + ephemeral runtime state; see adr_oci_referrers_signing_v1.md Amendment 3
    pub temp: TempStore,
    pub shims: ShimStore,           // older releases' `shims/…`, read by GC; deferred trees: `deferred(&G)`
    pub toolchain: ToolchainStore,  // the GLOBAL rendered home; a project home is a value, not a field (C-002)
    pub locks: PathBuf,
}
```

One instance per session. Sub-stores public fields. `root()` return OCX home path. `locks` is a
non-store field — the machine-global cross-process lock directory (`$OCX_HOME/locks`), never in
the GC graph. Locking policy (when to lock the data file in place vs. `lock_scoped` into `locks/`):
`arch-principles.md` "Locking Policy".

### Never Re-Construct a Store (Block-tier)

Never `<Store>::new(root.join("blobs"))` / etc. to reach a store — that
re-constructs a store `FileStructure` already owns, via a literal path join instead of the
canonical accessor. Always reach a store through the already-constructed `FileStructure`
(`FileStructure::with_root(root)` builds all nine stores in one place): `fs.blobs`,
`fs.layers`, `fs.packages`, `fs.index`, `fs.symlinks`, `fs.state`, `fs.temp`,
`fs.shims`, `fs.toolchain`, and `fs.deferred(&G)` for the deferred trees of one generation. A literal `.join(...)`
is fine when it is a genuine SUB-PATH under an already-correct store root (e.g. a scratch subdir
under `fs.temp.root()`) — the rule is against RE-CONSTRUCTING the store, not against every join.
The one sanctioned exception is the `--index`/`OCX_INDEX` override seam in `context.rs`, which
builds a second `IndexStore` pointed elsewhere instead of using `fs.index` — see "TagStore
deleted" above.

**Root-level state files under `$OCX_HOME`:**

| Path | Purpose |
|------|---------|
| `projects/` | Project GC ledger — flat directory of one symlink per registered project. Name = first 16 hex chars of `SHA-256(canonical_abs_project_dir)`; target = the project directory. Updated by `ProjectRegistry::register` after every `ocx lock` save. Read by `ocx clean` via `ProjectRegistry::live_projects()` to retain cross-project packages. ADR: `adr_project_gc_symlink_ledger.md`. **Two directories share the name `projects` — not to be confused with `state/projects/<key>/` below.** This one is the GC ledger: symlinks, lifetime tied to installs, read by `ocx clean` through `live_projects()`. `state/projects/<key>/` is the per-project shell-consent stamp root: deletable at any time, read only by its own sweep. Both key off the same `name_for_path(canonical_project_dir)` derivation but have opposite lifetimes and opposite failure modes (`adr_shell_env_overhaul.md` Decision 2). |
| `state/` | Persistent runtime state. Distinct from `cache/` (regenerable bulk). Subdirectory entries are small files whose existence or mtime IS the data — no content structure required. Never regenerated; persisted across sessions. |
| `state/update-check/<slug>` | Update-check throttle state file. `<slug>` = `to_slug(identifier)` — strict no-dot slug, for example `ocx_sh_ocx_cli` for `ocx.sh/ocx/cli`. File is always zero-byte; mtime is the only datum (time of last registry probe). Touched after every registry probe (success or error); NOT touched on throttle short-circuit. Parent directory created lazily on first touch. Written atomically via PID-suffixed temp file + `std::fs::rename`. |
| `state/patch-descriptors/<registry-slug>/<repo>.json` | Patch-descriptor discovery record — the three-state `__ocx.patch` key in a `BTreeMap<String,String>` tag→digest map (`FileStructure::patch_descriptor_path`). |
| `state/patch-companions/<registry-slug>/<repo>.json` | **Patch-tier companion pins** — the same tag→digest map shape, one ordinary tag key per pinned companion tag, holding the TOP (image-index) manifest digest (`FileStructure::patch_companion_path`). A companion is a package the user never named, so its binding lives here and NEVER in the local index (`subsystem-oci`: a pin moves only when named). Written by `install_companion` after the pinned pull succeeds; advanced only by `ocx patch sync`. Both patch state paths are keyed off the `FileStructure` root and never carry `--index` / `OCX_INDEX` redirection — `ocx patch test` therefore pins into its scratch root, which is where its compose reads back from. |
| `state/patch-state.stamp` | Zero-byte stamp; its mtime is the datum. Touched after any patch record write that changes a descriptor record or companion pin (`StateStore::patch_state_stamp()`). A member of the prompt hook's watch set, so an open shell reconciles at its next prompt. Not walked by `ocx clean`. Keyed off the `FileStructure` root, never `--index` / `OCX_INDEX`. |
| `state/projects/<key>/consent.json` | **Per-project shell-activation consent stamp** (`adr_shell_env_overhaul.md` Decision 2). `<key>` = the same `name_for_path(dunce::canonicalize(canonicalize(<project file>).parent()))` derivation the `projects/` ledger uses. `StateStore::project_state_dir(key)` / `consent_stamp_file(key)` / `project_state_root()`. Written by the consent-stamp allowlist — the toolchain-tier mutation commands (`add`/`remove`/`lock`/`update`/`run`/`pull`), plus `ocx init` for the project it creates and the explicit `ocx shell allow` — never by `ocx shell state` or `ocx self activate`, which are read-only. Membership is **suppressible, not conditional**: `OCX_NO_CONSENT` (and `--no-consent` on `init`/`pull`/`exec`, which outranks it) makes an allowlist member write nothing on a given invocation, resolved once in `app::project_context::record_activation_consent_over` and nowhere else — the set of commands that *may* write is unchanged, which is what A-29 constrains. `ocx shell allow` is outside that resolution by construction: it calls `project::consent::record` directly, so the explicit human gesture is never suppressible. Replaced atomically via `write_private_file_atomic`, never edited in place. **GC is the one exception to "`state/` is not walked by `ocx clean`" below**: swept iff the stamp deserializes at an understood `v` AND a pre-removal re-probe proves its own recorded `project_dir` is definitively absent — never derived from the `projects/` ledger, which the stamp does not consult. See `arch-principles.md`'s **State** glossary entry for the one-exception framing. |
| `state/projects/<key>/render_stamp.v2.json` | **Per-project toolchain render stamp** (C-003, D-V13; v2, `RenderStampV2`) — beside the consent stamp, same `<key>` derivation, same deletable-at-any-time lifetime, swept by the same `state/projects/<key>/` sweep. Carries `home`, a tagged `scope` (`project` with the canonical project dir, or `global`), `shell_dir` and `previous_shell_dir` (the two render directories `ocx clean` keeps), `fingerprint` (`render_fingerprint`, 64 hex), `generation`, the exposed name set and a bin fingerprint over `shells/<shell_dir>/bin` (records included). No field is `Option`-shaped and none covers mtime — a dropped key is a deserialize error, which reads as absent. A v1 `render_stamp.json` an older release wrote is read by the prompt gate and never written. |
| `state/render_stamp.v2.json` | **Global toolchain render stamp** — `StateStore::global_render_stamp_v2_file()`, directly under `state/` and never under `projects/<key>/`: `consent::record_in` refuses to stamp `$OCX_HOME` at all (A-44), and `ocx clean` classifies a `state/projects/<key>/` whose stamp records `$OCX_HOME` as `SweepReason::OcxHome` and removes the whole directory, which would delete the global stamp on every sweep. |

`$OCX_HOME/projects.json` and `$OCX_HOME/.projects.lock` from the prior JSON ledger are obsolete — safe to delete. `ocx clean` removes them opportunistically with a single debug log if encountered.

## Ten Stores

### BlobStore — Raw OCI blobs

Layout: `{root}/blobs/{registry_slug}/{algorithm}/{2hex}/{30hex}/data`

Each blob dir contain: `data` (raw blob bytes), `digest` (full digest string for recovery).

Key methods: `path(registry, digest)`, `data(registry, digest)`, `digest_file(registry, digest)`, `list_all() → Vec<BlobDir>`.

`BlobDir` accessors: `data()`, `digest_file()`.

### LayerStore — Extracted layers

Layout: `{root}/layers/{registry_slug}/{algorithm}/{2hex}/{30hex}/content/`

Each layer dir contain: `content/` (extracted files), `digest`.

Key methods: `path(registry, digest)`, `content(registry, digest)`, `digest_file(registry, digest)`, `list_all() → Vec<LayerDir>`.

`LayerDir` accessors: `content()`, `digest_file()`.

### PackageStore — Assembled packages

Layout: `{root}/packages/{registry_slug}/{algorithm}/{2hex}/{30hex}/`

**Repository NOT in path.** Only registry + digest set location → cross-repo dedup.

Each package dir contain: `content/`, `metadata.json`, `manifest.json`, `resolve.json`, `install.json`, `digest`, `refs/symlinks/`, `refs/deps/`, `refs/layers/`, `refs/blobs/`, `refs/origins/`, `refs/verified/`.

Key store methods: `path(pinned_id)`, `content(pinned_id)`, `metadata(pinned_id)`, `manifest(pinned_id)`, `resolve(pinned_id)`, `install_status(pinned_id)`, `digest_file(pinned_id)`, `metadata_for_content(content_path)`, `refs_symlinks_dir_for_content(content_path)`, `refs_deps_dir_for_content(content_path)`, `refs_layers_dir_for_content(content_path)`, `refs_blobs_dir_for_content(content_path)`, `resolve_for_content(content_path)`, `list_all() → Vec<PackageDir>`.

`PackageDir` accessors: `content()`, `metadata()`, `manifest()`, `resolve()`, `install_status()`, `digest_file()`, `refs_symlinks_dir()`, `refs_deps_dir()`, `refs_layers_dir()`, `refs_blobs_dir()`.

`*_for_content()` methods canonicalize path (follow install symlinks), navigate to sibling file/dir.

### IndexStore — Local index collection

Layout: `{root}/index/<source_slug>/{config.json, c/index.json, p/<ns>/<pkg>.json,
p/<ns>/<pkg>/o/<algo>/<hex>.json}` — the `index.ocx.sh`-hosted wire grammar verbatim, one subtree
per source (A2). `FileStructure.index` is the machine-local default; `--index`/`OCX_INDEX`
construct a second instance elsewhere instead (never through `FileStructure`).

`IndexStore` is pure wire grammar — root documents, catalog, dispatch objects
(`write_root`/`read_root`/`read_root_uncatalogued`, `write_dispatch_object`/`read_dispatch_object`,
`begin_catalog_transaction` → `CatalogTransaction`) — the actual local index `oci::index::LocalIndex`
reads and writes. There is no flat blob CAS in this store: config blobs and managed-config
payloads route exclusively through the machine-global `BlobStore` (`$OCX_HOME/blobs`) via
`ChainedIndex`'s attached content store — the same seam `recover_absent_dispatch` uses.
`ChainedIndex` recompute-verifies both directions (cache-read and post-source-fetch, CWE-345); a
present-but-corrupt entry is removed (`BlobStore::remove_blob`) before the source walk so the
write-through that follows a successful fetch heals it.

Outside the GC reachability graph — no `CasTier` variant, never walked by `ocx clean`. ADR:
`adr_index_indirection.md` Decision A2–A4, B1.

### ShimStore — Generated launcher directories (deferred tools, package entry points)

Layout of a deferred tool: `{root}/deferred/<G>/{registry_slug}/{repo_path}/{algorithm}/{2hex}/{30hex}/`
(`G` = `LauncherGeneration`, 16 hex; `fs.deferred(&G)`), then the `PackageDir` shape inside it: `l/` (one
native launcher `NAME[.exe]` plus its record `NAME.launch` of kind `deferred` per declared interface
name), `digest`, `refs/blobs/` (forward-refs keeping the closure's config blobs reachable). Trees an
older release wrote at `{root}/shims/…` (marker `bin/`, `.shimref`) are read by GC and never written.

A deferred directory exists precisely when a tool is composed onto `PATH` **without** its content
being materialized. Two divergences from its neighbours, both load-bearing:

- **The repository IS in the path**, unlike `PackageStore`, which keys on registry + digest alone
  for cross-repo dedup. A record names a *pinned identifier* and that identifier carries the
  repository, so two repositories resolving to one digest do not share a tree. Built with
  `repository_path()`, never a literal `/` join.
- **Launchers nest under `l/`, never at the tree root.** `binaries = ["digest", "refs"]` is a
  legal claim, so flat launchers would overwrite the CAS marker and the refs directory, and GC
  would then mis-classify liveness.

`l/` is also the **completeness marker**: the producer stages the tree in `fs.temp.scratch("deferred")`
(its lock file exists before the directory, so `ocx clean` skips it), writes `l/` last and publishes the
whole tree by `ocx_util::fs::publish::publish_dir`; a published tree is never edited. Complete means a
real directory holding every expected `NAME[.exe]` and `NAME.launch` as regular files. A name ending
`.launch` would collide with a record and gets no launcher. `ShimStore::list_all()` classifies on `l/`
(or `bin/` under `shims/`) and its walk is **unbounded in depth** — repository segment count is variable,
and GC collects whatever the walk fails to report, so a depth bound would silently delete live tools. No
child may be pruned by name either (`org/refs` and `org/l` are legal repositories).

Key methods: `path(pinned_id)`, `shim_dir(pinned_id) → ShimDir`, `list_all() → Vec<ShimDir>`.
`ShimDir` accessors: `root()`, `bin()` (the marker directory), `digest_file()`, `refs_blobs_dir()`.

In the GC graph as `CasTier::Shim`, but **rooted directly in the lock pins** rather than reachable
from a package: a deferred tree exists exactly when the package directory does not, so there is no
package to carry an edge to it. `ocx clean` retains a tree any registered project's `ocx.lock` pins;
`--force` drops those roots and collects it, and the next compose regenerates it. `deferred/<G'>` of
another generation is retired whole through `ocx_util::fs::publish::retire_dir` (`CleanKind::Launchers`).
Producer: `package_manager::tasks::prepare_lazy` (see `subsystem-package-manager.md`).

**Package entry points** live in the package directory: `PackageDir::launchers(&G)` =
`<package dir>/launchers/<G>/` holding `NAME[.exe]` + `NAME.launch` (kind `package`), written whole into the
temp package directory on pull or, when absent, staged in `fs.temp.scratch("launchers")` and published by
`publish_dir` on first use; `launchers_root()` is the parent. The directory a package's older release wrote
(`entrypoints/`) is composed only as the read-only-home fallback and is never written.

### ToolchainStore — The rendered toolchain home

Layout: `{root}/toolchain/` — depth 1 is a **closed, tree-owned set** (C-071): `.gitignore` (the
bytes `*\n`, C-004); `active`, a symlink (junction on Windows) to the current render directory
`shells/default-<16 random hex>/`, the **PATH-facing** prefix (C-078/C-079), switched by
`retarget_link` and never edited in place; `links/`, holding one `<group>/<entry>/` directory link
(junction on Windows) to a package root per selected group — every user-supplied name lives one
level below `links/`, never at depth 1 (C-072); and `shells/`, holding the **physical render
target** `shells/default-<16 hex>/bin/` — per exposed name a native launcher `NAME[.exe]` **plus** its
`NAME.launch` (kind `toolchain`), on every OS (DEFAULT group only). A render with new content publishes a
new directory and switches `active`; it deletes nothing, and `ocx clean` retires every `default-*`,
`.staging-*` and `.gc-*` child that is neither the stamp's `shell_dir` nor its `previous_shell_dir`. Depth 1 being closed by
construction is what retires the old `bin`/`.gitignore` reservation (C-073) instead of enforcing it
— `[group.bin]` renders at `links/bin/<entry>` and collides with nothing. Multi-shell selection is
out of scope ([#363](https://github.com/ocx-sh/ocx/issues/363),
[#189](https://github.com/ocx-sh/ocx/issues/189) stay open and undesigned); `shells/` holds the render
directories (`default` from older releases is never written, renamed or deleted).

**One grammar, two tiers.** `ToolchainStore` wraps the **global** home (`$OCX_HOME/toolchain`) and
forwards every accessor to a `ToolchainHome`, the value type that owns the shape — so store and
project home cannot drift into two layouts. A project's home is the same tree at a different root
and is deliberately NOT a `FileStructure` field: it depends on which project is in scope, so it is
resolved per call by `project::resolve_toolchain_home` (C-002), which is also where the
`toolchain-dir` config root applies. The global home ignores that key entirely (C-016).

Key methods: `home()`, `root()`, `bin()`, `shell_bin(shell)`, `gitignore()`, `ensure_gitignore()`,
`entry(group, entry)`, `links_group(group)`, `render_dir(name)`, `active_target_name()`. **Never join a group or entry name onto `root()`** —
`entry()` renders `<root>/links/<group>/<entry>` and `links_group()` renders `<root>/links/<group>`;
each validates every component and is a sanctioned route into the tree, returning
`ToolchainPathError` naming the refused `ToolchainPathComponent`. Neither `bin` nor `.gitignore` is
reserved as a group or tool name any more — `links`, `shells` and `active` are not reserved either,
since a group name is a component one level below every tree-owned name and cannot collide with
one. `bin()` stays the **PATH-facing** accessor (`<root>/active/bin`, through the `active` link) —
C-010's lookup-PATH exclusion is still derived from it, never a literal join. `shell_bin(shell)` is
the **render-target** accessor (`<root>/shells/<shell_dir>/bin`) the renderer writes, prunes,
fingerprints and guards through exclusively; the two are equal only through the `active` link,
never by string equality (C-078).

Outside the three GC tiers — `ocx clean` retires only the superseded render directories above, under the home's `toolchain-render` lock.
The tree is **derived state, not reference-counted**: a stale home is rebuilt by the next render.
Freshness is decided by the render stamp (`StateStore::render_stamp_v2`), never by a `refs/` walk — see the ARCH-4b sibling carve-out under
"symlink Module".

### SymlinkStore — Install symlinks

Layout: `{root}/symlinks/{registry_slug}/{repo_path}/candidates/{tag}` + `current`

```rust
pub enum SymlinkKind { Candidate, Current }
```

Key methods: `candidate(identifier)`, `current(identifier)`, `candidates(identifier)`, `symlink(identifier, kind)`.

`candidate()` use `identifier.tag_or_latest()` (fall back to `"latest"` if no tag).

### StateStore — Persistent runtime state

Layout: `{root}/state/` — see "Root-level state files under `$OCX_HOME`" above for the fixed
root-level entries (`state/update-check/<slug>` etc.); per-subsystem caches follow the general
shape `state/{subsystem}/{key}.json` (e.g. `state/referrers/<registry-slug>.json`,
`state/trust_root/<rekor-authority-slug>.json`, `state/host/capabilities.json` — the Linux-only
host libc-detection cache, one flat file with no per-key sharding since it is keyed on the local
machine rather than a registry).

Key methods: `root()`, `update_check_dir()`, `update_check_file(identifier)`; signing/trust caches: `referrers_capability_file(registry)` (`state/referrers/<registry-slug>.json`) and `trust_root_file(rekor_authority)` (`state/trust_root/<rekor-authority-slug>.json`) — both slug via `to_relaxed_slug` (dots preserved) and are the layout owners for the OCI referrer capability + offline-verify trust-root caches; managed-config tier: `managed_config_dir()`, `managed_config_snapshot_file()` (metadata JSON) + `managed_config_toml_file()` (readable `config.toml` payload sibling) — the snapshot persists as **two** files (payload written first, metadata last, each its own atomic temp+rename; metadata absent ⟹ whole snapshot reads absent), `managed_config_refresh_marker()` (zero-byte throttle marker), `managed_config_pause_file()` (content-bearing `pause.json` written by `ocx config update --pause`), `patch_state_stamp()` (zero-byte patch-record change stamp), plus the pure associated `managed_config_snapshot_path(ocx_home)` and `managed_config_toml_path_for_snapshot(snapshot_path)` (sibling derivation) shared with the config loader. Generic throttle primitives (promoted from `package_manager/tasks/update_check.rs`): `is_throttled(path, interval) -> bool` (sync, blocking I/O) and `touch(path) -> impl Future` (async, atomic write via temp+rename, logs failure at debug — never propagates). Callers own the state-file path (e.g. via `update_check_file`); these two methods are path-agnostic. Toolchain render stamps (C-003): `render_stamp_v2(target)` /
`set_render_stamp_v2(target, stamp)` take a `RenderStampTarget` so a tier cannot be stamped at the
other tier's path, over the ungated path accessors `render_stamp_v2_file(key)`
(`state/projects/<key>/render_stamp.v2.json`) and `global_render_stamp_v2_file()`
(`state/render_stamp.v2.json`). An unusable stamp — absent, corrupt, unknown `v` — reads as absent at
debug level and the caller's answer is "re-render".

**Definitional contract** (applies to every `state/{subsystem}/...` entry — e.g. the OCI
Referrers capability cache and the offline-verify trust-root cache, both introduced by
`adr_oci_referrers_signing_v1.md` Amendment 3):

- **Purpose:** ephemeral, non-content-addressed, registry-scoped or subsystem-scoped runtime state. NOT for content (use `blobs/`), extracted files (`layers/`), assembled packages (`packages/`), persistent metadata mirror (`tags/`), or install pointers (`symlinks/`).
- **Lifetime:** TTL-bound per subsystem (the Referrers capability cache uses a flat 6h TTL, ADR Amendment 6). Stale entries are safe to delete at any time without integrity loss.
- **GC:** **not walked** by `ocx clean`, with exactly **one exception**: `state/projects/<key>/` (the shell-activation consent stamp and the project render stamp, see "Root-level state files" above) is swept on the stamp's own recorded `project_dir` liveness, one call site in `clean.rs` — never on `refs/` reachability and never on the `projects/` ledger, which this sweep does not consult (`adr_shell_env_overhaul.md` Decision 2). Every other `state/{subsystem}/...` entry has no refs, no digest, no GC role. v2 may add `ocx clean --state` to truncate the rest.
- **Atomicity:** writes via `tempfile::NamedTempFile` + `std::fs::rename` (Windows-safe across existing targets). The `tempfile::persist` shortcut does **not** replace-existing on Windows.
- **Concurrency:** advisory file lock optional per subsystem. Capability cache reads tolerate fail-open ("file missing → unknown, reprobe").
- **Schema:** each subsystem owns its JSON schema. No registry-wide invariants beyond filename layout.

### TempStore — Download staging

Layout: `{root}/temp/{32-hex-hash}/` — deterministic SHA-256 hash of full identifier string.

Lock file sits as sibling: `{32-hex-hash}.lock`. `try_acquire()` non-blocking; stale artifacts cleaned on acquire. Temp dir atomically renamed into `packages/` on completion.

Export and import archive staging also live under `temp/`, in uniquely named directories whose lock file is held exclusively for the run, so `ocx clean` leaves them alone.

## Path Construction

### Slugification

All OCI identifier parts slugified via `to_relaxed_slug()`: keep `[a-zA-Z0-9._-]`, replace rest with `_`. Example: `localhost:5000` → `localhost_5000`.

### Repository Path Splitting

**Never use `.join("org/project/tool")`** — embed literal `/`, cause mixed separators on Windows. Use `repository_path()` — split on `/`:

```rust
pub(crate) fn repository_path(repository: &str) -> PathBuf {
    repository.split('/').collect()
}
```

### Digest Sharding (`ocx_oci::cas_path`)

`ocx_oci::cas_path::cas_shard_path(digest)` produce `{algorithm}/{hex[0..2]}/{hex[2..32]}` — 2-level shard, 32 hex chars total in path. Full digest NOT recoverable from path alone; written to sibling `digest` file by `write_digest_file()`.

`CAS_SHARD_DEPTH = 3` (algorithm + prefix + suffix). Store walkers set `max_depth` to `1 + CAS_SHARD_DEPTH` (extra 1 = registry slug level).

`CasTier` enum: `Package`, `Layer`, `Blob` — used by GC and reporting.

## ReferenceManager

Manage install symlinks and cross-tier forward-refs for safe GC. **Always use this for install symlinks, never raw `symlink::update/create`.**

```rust
pub fn link(&self, forward_path: &Path, content_path: &Path) -> Result<()>
pub fn unlink(&self, forward_path: &Path) -> Result<()>
pub fn link_dependency(&self, dependent_content: &Path, dependency_content: &Path, dependency_digest: &oci::Digest) -> Result<()>
pub fn unlink_dependency(&self, dependent_content: &Path, dependency_digest: &oci::Digest) -> Result<()>
pub fn broken_refs(&self) -> Result<Vec<PathBuf>>
```

**Arg order for `link()` is `(link, target)` — opposite of `symlink::update(target, link)`.** Common confusion source.

Install back-reference: `packages/.../refs/symlinks/{16_hex}` → forward symlink path. Name from `name_for_path(forward_path)` = first 16 hex chars of SHA-256(path bytes).

Dependency forward-ref: `packages/.../refs/deps/{algorithm}_{32_hex}` → dependency's `content/`. Name from `cas_ref_name(digest)` (in `cas_path` module, re-exported from `file_structure`) = `"{algorithm}_{first_32_hex}"`. No back-ref in dependency's `refs/symlinks/`.

Layer forward-ref: created by `pull` directly (not via `ReferenceManager`). Symlink in `refs/layers/` target `layers/.../content/`. GC recover layer entry dir via `.parent()` on target.

Blob forward-ref: created by `pull` directly. Symlink in `refs/blobs/` target `blobs/.../data`. GC recover blob entry dir via `.parent()` on target.

Origin marker: `packages/.../refs/origins/{name_for_path(origin)}` — a **regular file** (not a symlink), content = the canonical `<registry>/<repository-path>` origin string, written by `record_origin()` on a genuine fetch only. Records provenance (which repository this host resolved this digest under — logical, not transport), not liveness — deliberately outside the GC reachability graph below, so `refs/origins/` is never walked for roots or edges. Read side: `PackageDir::recorded_origins()`. Consumed by `project::consent::verified_sources` (the shell-activation namespace-grant predicate).

Verified marker: `packages/.../refs/verified/{name_for_path(origin)}` — same shape as the origin marker, written by `record_verified()` only after a publisher round ran and passed for that logical repository (never for an undeclared, `--no-verify` or digest-without-committed-root install). Read side: `is_verified()`, one marker, never a listing; the origin gate on `find` (`origin_gate_open`) reads it for a package its index declares publishers for. Outside the GC graph like `refs/origins/`.

`broken_refs()` check only `refs/symlinks/` — not `refs/deps/`, `refs/layers/`, `refs/blobs/`.

**Idempotent**: `link()` no-op if forward already points to content. `unlink()` no-op if forward absent.

## GC Safety

GC (`garbage_collection/reachability_graph.rs`) build `ReachabilityGraph` covering all three CAS tiers in single BFS pass. Packages with live `refs/symlinks/` entries or digests pinned by any registered project's `ocx.lock` = roots. BFS follow four edge types from each package: `refs/deps/` (dependent packages), `refs/layers/` (extracted layers), `refs/blobs/` (raw blobs). Layers and blobs no outgoing edges — reachable only via package refs. Unreachable = collected.

Project-registry roots read via `ProjectRegistry::live_projects()` at the start of `ocx clean`. The ledger is the flat symlink store at `$OCX_HOME/projects/` (ADR: `adr_project_gc_symlink_ledger.md`). Each symlink's liveness is a three-state probe (`Live`/`Dead`/`Unknown`): a broken or lock-absent link is pruned silently at debug; a transiently unreachable link (`Unknown` — NFS/automount) is retained with one WARN and excluded as a root this run. There is no parse surface to corrupt — the prior `projects.json` JSON ledger and its corrupt-registry exit-78 branch are eliminated. Pass `--force` to `ocx clean` to ignore the registry and collect all packages not held by live symlinks.

Blobs first-class BFS entries: every `CasTier` variant (`Package`, `Layer`, `Blob`, `Shim`) included in reachability walk. Previous `tier != CasTier::Blob` skip removed; blobs retained only when live `refs/blobs/` symlink points to them.

Shims registered with the **package** shape — entry *and* outgoing edges (`refs/blobs/`) — but their liveness has no package edge to inherit: a shim exists exactly when the package directory does not, so a lock pin roots the shim directory directly. Lock-pin presence is therefore satisfied by *either* tier, `packages/` or `deferred/<G>/` (or an older release's `shims/`); a package-only presence gate silently drops a deferred tool out of the root set and collects a live shim on the next sweep.

`BlobStore::write_blob` / `read_blob` (stateless) are the only blob-IO entry points. Writes use `tempfile::NamedTempFile::new_in(parent)` + `sync_data` + atomic rename to the CAS path; content-addressed invariant (same digest ⟹ same bytes) means concurrent writers produce byte-equivalent output and the rename is idempotent. On Windows, `persist_with_windows_retry` wraps the rename with exponential backoff (3 retries: 100/400/800 ms ±25 % jitter) on `ERROR_SHARING_VIOLATION` (32) and `ERROR_ACCESS_DENIED` (5) — Windows Defender real-time scanning on `windows-latest` GitHub Actions runners is the dominant source of these transients (rattler `rename_with_retry` precedent). After retry exhaustion the function re-checks existence and returns Ok if the path now exists (idempotent recovery).

Same-process dedup for concurrent same-digest fan-out lives in `package_manager::tasks::pull_local::PullCoordinator`, which owns a `singleflight::Group<oci::Digest, ()>` scoped per pull operation. The OCI manifest layer-download path is the only caller with concurrent same-digest writes; index-layer callers (`write_manifest_blob`, `ChainedIndex::fetch_blob` write-through) call `BlobStore::write_blob` directly without dedup — they are sequential and the content-addressed invariant covers any rare cross-process race.

There is no `LockFileEx` on blob `data` files. The F1 class (cross-process raw reader of a locked range → `ERROR_LOCK_VIOLATION`) is structurally impossible — see ADR `adr_file_lock_unification.md` §Decision 2.

## symlink Module

Low-level primitives. Use only for non-package symlinks or inside `ReferenceManager`:
- `create(target, link)` — create symlink + parent dirs
- `update(target, link)` — create or replace
- `remove(link)` — remove; no-op if absent
- `is_link(path)` — use instead of `is_symlink()` — handle Windows NTFS junctions

Windows: use NTFS junction points (no privilege escalation needed). `retarget_link` switches `toolchain/active` by one rename of a staged link on Unix and an in-place junction retarget on Windows (variant A, proven by the `junction-proof` CI job; variant B′ keeps `symlink::replace_atomic`).

**ARCH-4b — sanctioned exception for `$OCX_HOME/projects/`:** `ProjectRegistry` uses `symlink::create`/`update` (via `symlink::replace_atomic`) directly, bypassing `ReferenceManager`. This is intentional and must not be "fixed":

- `projects/` links target an absolute path **outside** `$OCX_HOME` (the project directory), and `ReferenceManager::link()` has **no containment check** to stop it: it derives the back-ref path through `PackageStore::refs_symlinks_dir_for_content`, which canonicalizes, strips a trailing `content` component and joins `refs/symlinks` — pure sibling navigation, with nothing asserting the result is under `$OCX_HOME/packages`. Routing a `projects/` link through it would write a `refs/symlinks/` directory into the user's own tree.
- `projects/` links are categorically not install back-refs. They are GC-root registrations with a flat-symlink liveness model (ADR: `adr_project_gc_symlink_ledger.md`).
- Any future reviewer who sees raw `symlink::` calls in `project/registry.rs` should recognise this carve-out, not re-flag it as a violation of the "always use `ReferenceManager`" rule.

**ARCH-4b sibling — no back-reference for a toolchain link (C-052):** the `links/<group>/<entry>` links a render writes into a toolchain home also use `symlink::update` directly, and for the *opposite* reason to `projects/`. Their targets ARE package roots, so `ReferenceManager::link()` would navigate cleanly to exactly that package's live `refs/symlinks/` — and a back-reference is a GC root, so the link would pin the package forever, on every project that ever rendered a tree. The toolchain home is derived state rebuilt by the next render, never a reference-counted install (see `ToolchainStore` above), so it is exempt from reference counting by design: it takes no forward-ref, no back-ref, and no `refs/` edge of any kind. Reaching for `ReferenceManager` in `tasks/render_toolchain.rs` is the right rule against the wrong tree.

## hardlink Module

Low-level primitives for file-level dedup during layer assembly:
- `create(source, link)` — hardlink; create parent dirs; fail if link exists or cross-device
- `update(source, link)` — create or replace

Cross-device hardlinks fail with `io::ErrorKind::CrossesDevices`. `$OCX_HOME` must sit on single volume — required by `temp → packages/` atomic rename.

**Vendor layout**: `file_structure::vendor_layout` owns the path math of an `ocx vendor` directory and `StagedTree` (a `mkdtemp` 0700 directory beside `DIR`, renamed into place). The assembly walker takes `LinkMode::{Hardlink, Copy}`; vendoring uses `Copy`, writing data and the masked mode only and keeping the file mtime.

**Assembly walker**: `file_structure::assemble_from_layer(source_content, dest_content)` mirror layer's `content/` tree into package's `content/` dir — hardlink regular files via `hardlink::create`, create real subdirs, recreate intra-layer symlinks verbatim. `packages/{P}/content/` is real dir, not symlink into `layers/`. Walker fan out dir-level tasks through semaphore-bounded `JoinSet`; per-task stats return-and-summed (no shared mutex). Windows layer symlinks return `io::ErrorKind::Unsupported`.