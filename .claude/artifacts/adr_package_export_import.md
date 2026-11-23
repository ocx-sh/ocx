# ADR: Package export and import over blob directories

## Metadata

**Status:** Accepted (2026-11-23, under goal-loop authority, `.agents/goals/adr-package-export-import.md` § Autonomy; planned in `plan_package_export_import.md`)
**Date:** 2026-11-23 (review round 1 applied the same day)
**Deciders:** owner (decisions recorded in the discussion), architect worker (`/hex-architect`, tier xhigh, unattended goal loop)
**Reversibility:** mixed, stated per surface.

| Surface | Door | Why |
|---|---|---|
| Export directory layout (`index/`, `blobs/`, `ocx-export.json`, `referrers/` markers) | One-way | Every later ocx must read an export an earlier ocx wrote. The marker's `format_version` is the only exit. |
| Report roots `export` and `import` | One-way | Gated machine documents under `adr_ocx_interface_contract.md`; a break needs a ledger entry and a version bump. |
| Commands, flags, `OCX_BLOB_DIR`, `OCX_BLOB_CACHE_DIR`, `[blobs]` | Two-way before 1.0 | Interfaces, so a change is a decision and a changelog line, like every other pre-1.0 interface. Not cheaper than that. |

**Blast radius:** external contract. It reaches `ocx_oci`, `ocx_index`, `ocx_store`, `ocx_sign`, `ocx_config`, `ocx_env`, `ocx_project`, `ocx_package_manager` and `ocx_cli`. A later Bazel track reads the same blob layout.
**Domain tags:** data, integration, security, devops
**Tech strategy alignment:** golden path. Rust 2024 and Tokio, no new dependency, no new infrastructure.

**Run configuration.** Tier xhigh. Research axes:

| Axis | Status | Evidence |
|---|---|---|
| Prior art (extra local sources and a writable cache) | Skipped, already researched | `.claude/artifacts/research_local_sources_cache_config_prior_art.md` |
| Codebase seams | Skipped, already researched | `.claude/artifacts/discover_package_export_import.md`, `.agents/research/research_ocx-config-and-blob-seams.md`, plus the code reads cited per decision |
| Concurrent writers into one CAS cache directory | Researched in this run | `.claude/artifacts/research_concurrent_blob_cache_writes.md` |

**Required artifacts:** this ADR; `.claude/artifacts/system_design_package_export_import.md` (C4, contracts, schemas, STRIDE, test map, work packages).

**Input:** `.agents/discussions/bazel-hermetic-package-bundles.md`, block "Track 1 as it stands on 2026-11-22". That block wins over every earlier version of this ADR, except where a decision below names a divergence and its reason.

**Vocabulary.** "Bundle" is not used for this feature. `Metadata::Bundle` already names a package's wire format, so the word would carry two meanings. The nouns are *export* (the directory), *blob directory* (a read-only lookup root) and *blob cache* (the one writable lookup root).

## Context

Moving packages between machines has no first-class route today.

- An air-gapped machine is served only through a registry mirror. The home cannot be copied and re-verified, because it keeps no compressed layer archives. [ocx-sh/ocx#23](https://github.com/ocx-sh/ocx/issues/23) was closed with "requires dedicated import/export command".
- CI caches no OCX state. Each job downloads every layer archive again.
- A later Bazel track wants a blob it can find from a registry and a digest, with no lookup document in between.

An earlier version of this ADR used one OCI image layout and a Bazel fetch plan. The owner reopened Track 1 on 2026-11-22 and gave two reasons for the new shape.

1. **Registries must not share a namespace.** The owner's security policy forbids overlap between registries. One flat blob pool lets a package of registry B be satisfied by a blob only registry A shipped. The home's `blobs/<registry>/` split already prevents that.
2. **One lookup chain for index and blobs.** Metadata blobs are already read from the home, then the source. The owner wants package data on the same chain: extra blob directories after the home and before the network, each read checked by digest. File, HTTP and OCI blob sources may follow later.

Two further points from the same block shaped the format. The home already names every blob by registry and digest, so reusing it removes a translation layer. The fetch plan tied ocx to one consumer's store model, so it was dropped. The cost was accepted: oras and skopeo cannot read an export.

## Decision drivers

1. **Registry isolation.** No blob reached under registry B may come from bytes only registry A shipped.
2. **Offline completeness.** After an import, tools run with the export deleted and the network blocked.
3. **Pin discipline.** A local pin moves only under a command that names what to move. An import without names never moves an existing pin; naming a package lets its pins move.
4. **Integrity on every read.** A changed byte is refused, and the refusal does not depend on the network being reachable.
5. **One lookup order everywhere.** Pull, export, copy and verify read blobs through one chain.
6. **Reuse.** The local index writers, the refresh machinery and the layer pipeline already exist.
7. **Bounded surface.** Few new names, each matching an existing precedent.

## Considered options

### Export format and lookup shape

| Option | Description |
|---|---|
| **A** | One OCI image layout: `oci-layout`, `index.json` with `ref.name` per tag, flat `blobs/<alg>/<hex>`. Withdrawn by the owner. |
| **A′** | One OCI image layout per registry: `DIR/<registry>/{oci-layout,index.json,blobs/<alg>/<hex>}`. |
| **B** | Home layout: `index/<source>/…` plus `blobs/<registry>/<alg>/<2hex>/<30hex>/data`. Blob directories chain into lookup; the index never chains. |
| **C1** | One "data directory" that chains both: index sources and blobs read through one ordered list. |
| **C2** | A separate transfer format (a flat archive set plus a manifest), translated into the home on import (Cargo `local-registry` style). |

Weights sum to 1.0. Scores 1 to 5, 5 is best.

| Criterion | Weight | A | A′ | B | C1 | C2 |
|---|---|---|---|---|---|---|
| Registry isolation (no cross-registry blob satisfaction) | 0.15 | 1 | 5 | 5 | 5 | 3 |
| Offline completeness, integrity on every read | 0.15 | 4 | 4 | 5 | 5 | 4 |
| Pin discipline (no pin moves unless named) | 0.15 | 4 | 4 | 5 | 1 | 4 |
| Reuse of existing writers, readers, refresh rules | 0.15 | 2 | 2 | 5 | 3 | 2 |
| Bazel track fit (blob from registry + digest, no lookup document) | 0.10 | 3 | 5 | 5 | 5 | 2 |
| Surface size (new names, new formats) | 0.10 | 2 | 2 | 4 | 3 | 1 |
| Ecosystem tooling reads it (oras, skopeo) | 0.10 | 5 | 4 | 1 | 1 | 1 |
| Security surface (hostile directory parsing) | 0.10 | 3 | 3 | 4 | 2 | 3 |
| **Weighted total** | 1.00 | **2.95** | **3.65** | **4.40** | **3.20** | **2.65** |

- **A** fails registry isolation outright: a flat pool answers any registry from any registry's bytes.
- **A′** is the fair rival. Each per-registry layout is a valid OCI layout, so `oras cp --from-oci-layout DIR/<registry>:tag` works, and blob paths follow from registry plus digest. It loses on reuse: every `index.json` duplicates the local index, and import must translate `ref.name` rows into local-index roots. That translator is the second format this ADR exists to avoid.
- **C1** breaks pin discipline. A chained index answers a tag from a directory nobody named, the "silent pin" the project rules forbid.
- **C2** adds a format and a translator. It decouples from home internals, but nothing here needs that.

**Prior art for B.** Go serves its module cache as a proxy: `GOPROXY=file://$GOMODCACHE/cache/download` makes the download cache a read-only module source for another build. B is the same move, with a digest check on every read.

**Risks of B.** The home layout becomes a persisted format. Its registry key is `to_relaxed_slug(registry)`, which is lossy (`localhost:5000` becomes `localhost_5000`). The shard path keeps 32 of 64 hex digits, so readers verify and never trust a path. Decision 1 adds the marker that makes the slug reversible, and Decision 8 the full-digest rule.

### Settings shape

| Option | Description |
|---|---|
| **S1** | An ordered read-only list of blob directories, plus at most one separate writable cache (`dirs`, `cache_dir`). |
| **S2** | One ordered list with a per-entry mode (`path:rw`, ccache `read-only`). |
| **S3** | Separate read and write lists (BuildKit `--cache-from` / `--cache-to`). |

| Criterion | Weight | S1 | S2 | S3 |
|---|---|---|---|---|
| Write target never ambiguous | 0.30 | 5 | 3 | 2 |
| Order explicit and documented | 0.20 | 5 | 4 | 2 |
| Fits existing env and flag grammar (`PathList`, one value type per flag) | 0.20 | 5 | 1 | 4 |
| Expressiveness (several writers, per-source modes) | 0.10 | 2 | 5 | 5 |
| Surface size | 0.20 | 4 | 3 | 3 |
| **Weighted total** | 1.00 | **4.50** | **3.00** | **2.90** |

- **S2** needs a value grammar inside a path list. A path containing `:rw` becomes unspellable, and IC-12 forbids two value types per flag.
- **S3** allows several write targets, so a fetch's write side becomes a policy question.
- **S1** matches Git alternates, XDG and Go `GOMODCACHE` (prior-art pattern 1). Its cost: a read-only directory is never written through; export is the explicit writer.

**Recommendation:** B with S1.

## Decision

### Decision 1 — Export directory layout and the root marker

An export directory `DIR` holds a marker and two subtrees in the home's grammar:

```
DIR/
  ocx-export.json
  index/<slug(source)>/config.json
  index/<slug(source)>/c/index.json
  index/<slug(source)>/p/<ns>/<pkg>.json
  index/<slug(source)>/p/<ns>/<pkg>/o/<alg>/<hex>.json
  blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/data
  blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/digest
  blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/referrers/<alg>-<hex>
  blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/referrers/.complete
  blobs/.tmp/
```

**The marker.** `DIR/ocx-export.json` is `{"format_version": 1, "registries": {"<slug>": "<registry as written>"}}`. It carries no inventory of contents.

- **Why not `config.json`.** `index/<source>/config.json` is `IndexFormatConfig`, whose field order is wire: `ocx-sh/index`'s renderer emits the same order, so a field added there breaks it (`crates/ocx_index/src/wire.rs:30-39`). It also exists per index source, not per blob root.
- **No marker means layout 1.** A blob directory without a marker reads as `format_version` 1. Blob lookup never reads the marker: a blob path follows from the registry the caller already holds.
- **Import requires it.** Import maps index and blob directories to registries only through `registries` (Decision 3). An export with no marker is `export_dir_invalid`, naming "re-run export".
- **The cache carries one too.** `CACHE/ocx-export.json` has the same grammar and maps the cache's blob root. It is advisory: written best effort, never read by lookup.

**Divergence, recorded.** The discussion's Track 1 summary says an export has "no feature-specific format" (discussion line 168). The marker is one, on purpose. Two reasons: the relaxed slug is lossy, so without the marker a directory name cannot be mapped back to a registry. And import into an empty home has no configured namespace to match against. The marker is the smallest file that makes the layout reversible.

**Registry key and case.** `<slug(registry)>` is the identifier's registry as written, never a mirror host. Registries `a` and `b` with the same repository path stay apart. `PackageRef` keeps the registry's case: only the repository is refused for uppercase (`crates/ocx_oci/src/package_ref.rs:296-321`). So `Reg.test` and `reg.test` are different keys, and they collide on a case-insensitive filesystem.

- **The writer refuses a slug collision.** Two different registries whose slugs are equal after ASCII case folding cannot share an export. The second is `export_dir_invalid`, naming both registries. This also covers `a:5000` against `a_5000`, which the relaxed slug folds together.
- **Comparison is ASCII-case-insensitive** wherever a registry is matched against the marker, because host names are. `consent::source_of` already lowercases the registry for the same reason (`crates/ocx_project/src/consent.rs:186-229`).

**What `blobs/` holds.** Image indexes, manifests, configs, layer archives, referrer manifests and the blobs they reference. A home's `blobs/` holds no layer archives; an export's does.

- `referrers/<alg>-<hex>` is an empty marker named by a referrer manifest digest. The grammar is Decision 5's.
- `referrers/.complete` is written once the subject's referrer listing was read in full, even when it was empty. So "this subject has no referrers" differs from "its referrers were not exported".
- `blobs/.tmp/` holds in-flight writes. No reader scans it.

A root `ocx export` writes `ocx-export.json` and `blobs/`, and no `index/`. A lockfile records leaf digests per platform, not image indexes, so it cannot back a faithful dispatch object (`crates/ocx_project/src/lock.rs:133-150`). Lock-pinned resolution is index-free, so `blobs/` is enough. Root import is Decision 11.

### Decision 2 — One lookup order, blobs only

Every digest-addressed read walks one order:

**home → blob directories in list order → blob cache → network.**

- The home step is each seam's existing read: `layers/` for a layer (`pull.rs:925-930`), the attached content store for metadata (`ChainedIndex::fetch_blob`).
- The index never chains. Tags resolve only from the local index home (`--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index`). A blob directory never answers a tag.
- The cache never holds index documents.
- `--offline` stops before the network step and keeps every local step.
- Each blob's serving origin is logged at debug level: `home`, `dir[<n>]`, `cache` or `network`.

**Digest check on every read.** Metadata is read under its existing cap and hashed before use. A layer stream is hashed by the existing pipeline, which also drains and checks completeness.

**Mismatch policy: order changes cost, never the result.** A bad copy is never the answer, and a bad copy in one source never stops a good copy in a later one.

| Source | On a mismatch or a short read |
|---|---|
| Blob directory | Warn, remember the mismatch, continue with the next source. The directory is never written. |
| Blob cache | Treat as a miss: delete the entry and continue. If the delete fails, warn and continue. |

- **Final verdict.** If no source yields a clean copy and a blob directory yielded a bad one, the read fails with `digest_mismatch` (exit 65). This holds at every end of the walk: a network error, `--offline`, and import, which has no network step. So "a changed byte is refused (exit 65)" holds with the network blocked.
- **Cache-only mismatches are misses.** When only the cache held a bad copy, the walk ends as if the cache had missed: the network error, or `offline_mode` (81) offline. The 81 message says how many cache entries were discarded on the way. Reason: a cache is self-healing and may be shared, so its bad entry says nothing about the data the user named.
- **Short reads.** A blob-directory file shorter than the descriptor is a bad copy, so the verdict is 65; a short cache file is a miss. `ShortBlobRead` (75, `crates/ocx_oci/src/client/error.rs:37`) stays for the network, where a retry can help.
- **Absent everywhere at import.** A blob no source holds, with no bad copy seen, is `export_not_found` (79) at import, naming the blob and the package. A truncated export that lacks a layer of an included platform ends here.
- **Layer retries.** A layer whose stream failed at its end restarts into a fresh output directory, never on top of the partial one.

### Decision 3 — Check 1: how import points the refresh at the export

**Question.** How does `ocx package import` refresh the home index from `DIR/index` with no config edit?

**Evidence.**
- `OcxIndexConfig` takes a boxed `IndexTransport` directly; `OcxIndex::new` does not call `resolve_base_url` (`crates/ocx_index/src/ocx_index.rs:390-570`).
- `FileIndexTransport::new(base_url, root)` serves any directory, with a document read cap (`crates/ocx_index/src/file_transport.rs:114`, `:165`).
- `Index::from_source(OcxIndex)` wraps one source (`crates/ocx_index/src/lib.rs:168`).
- `OcxIndex::source_kind` is always `Published` (`ocx_index.rs:1251`). The home's kind per registry comes from the configured chain (`ChainedIndex::kind_for_registry`, private, `chained_index.rs:201`).
- `IndexRoot.repository` is a required string (`wire.rs:54-56`), so a root always carries a routing pointer.

**Decision.**
- **Mapping only through the marker.** Each `DIR/index/<dir>/` maps to the registry `ocx-export.json` lists for `<dir>`. A directory the marker does not list, or a marker entry whose slug does not equal `to_relaxed_slug(registry)`, is `export_dir_invalid` (65).
- **Cross-check against named identifiers.** A named identifier's registry must equal a marker registry, ASCII case folded. A named registry the marker lacks is `export_not_found` (79). The command-line, configured-namespace and derived-pointer ladder of the previous draft is deleted.
- **One source per directory.** Import builds an in-memory `OcxIndex` over `FileIndexTransport::new("file:///<abs>/index/<dir>", …)`, wrapped by `Index::from_source`.
- **Kind comes from the importing home, never from the export.** A new public `Index::source_kind_for(registry)` exposes `kind_for_registry`. A derived registry stays derived. A registry no configured source serves is `Derived`: `kind_for_registry` falls back to `SourceKind::Derived` (`crates/ocx_index/src/chained_index.rs:196-207`). Import reads the kind from the home's configured source list before it builds its offline view.
- **`LocalIndex::import_tags(identifier, source, kind, plan)`** writes rows only for the tags its plan admits (Decision 10). It reuses `refresh_published` for `Published` and `refresh_derived` for `Derived`, with `RootScope::Tags`.
- **Routing is never adopted from the export.** An existing home root keeps its `repository` pointer. A new root gets `oci://<registry>/<repo>`, the identity pointer `refresh_derived` already authors. For a published namespace that placeholder is replaced by the next `ocx index update` from the configured source. Offline resolution by tag never dials, so it needs no routing.
- **Fan-out bound.** The import task lives in `ocx_package_manager` and cannot name the CLI's `pub(super) INDEX_REFRESH_CONCURRENCY` (`crates/ocx_cli/src/command/index_common.rs:16`). The constant moves to `ocx_index::PACKAGE_REFRESH_CONCURRENCY` (value 8, unchanged). `index_common.rs` uses it from there, and its structural test (`index_common.rs:209`) is re-pointed at the new name.

**Reason.** The marker is the only mapping that works in an empty home and cannot be steered by what a home happens to configure. Kind and routing from the home keep an export from turning a plain registry into a published one, or a package into a redirect.

### Decision 4 — Check 2: the pin snapshot an export writes

**Question.** The home's root lists every tag it knows. Copied verbatim, a no-names import would pin tags nobody exported.

**Evidence.**
- `LocalIndex::commit_published_root` with `RootScope::Tags(&[..])` writes a root holding only the listed tags (`local_index.rs`, `merge_root`).
- The published catalog entry is `sha256(root bytes)`, authored by the writer (`store.rs`, `root_catalog_entry`).
- `fetch_catalog_strict` needs `c/index.json` (`ocx_index.rs:994`).

**Decision.**
- **Tag resolution.** Export resolves each named tag through the normal chain, exactly as `ocx package pull` does: the local index, then the network unless `--offline` or `--frozen`. A first-sight tag is committed to the home index on the way, as a pull commits it. Under `--offline` a tag the home does not pin is the existing `index_resolution_blocked` (81, `PolicyResolutionBlocked`, `crates/ocx_index/src/chained_index.rs:313-324`). The snapshot is then written from the home, so the export and the exporting home cannot disagree. Reason: one resolution rule for pull and export, and no "run `ocx index update` first" step.
- **Writers.** Export writes `DIR/index` with `IndexStore::new(DIR/index).with_locks_root($OCX_HOME/locks)` through `LocalIndex` refresh, with `RootScope::Tags(exported tags)`.
- **Published shape always.** Every source directory carries `config.json` and an authored `c/index.json`, so a no-names import enumerates through `fetch_catalog_strict`.
- **Restricted root.** A tagged identifier exports that tag; a bare one exports `latest` (`tag_or_latest`); a digest identifier writes no pin, only blobs.
- **Catalog re-authored** over the restricted bytes, so the export is self-consistent.
- **Re-export.** Writing into an existing `DIR` merges per package: rows union, and a tag the exporter now pins elsewhere is replaced and reported `replaced`. Decision 8 defines the commit point.
- **Image indexes verbatim.** A dispatch object is copied byte for byte, so it lists every platform the publisher has. Blobs are present only for the exported platforms (Decision 9).

**Verified against the format code:** `store.rs` path helpers and `root_catalog_entry`, `local_index.rs` `RootScope` and `merge_root`, `wire.rs` `IndexFormatConfig`, `CatalogDocument`, `IndexRoot`.

### Decision 5 — Check 3: one lookup type, referrer markers, and copy

**Evidence.**
- `OciTransport` is sealed inside `ocx_oci` (`client/transport.rs:256`).
- `ocx_oci` may depend only on `ocx_util`, `ocx_console`, `ocx_exit`, `ocx_env`; `ocx_store` depends on `ocx_oci` (`scripts/crate_map.toml:28-33`).
- The layer pipeline sees only a physical identifier after routing (`client.rs:846-916`). The logical registry is known one level up (`pull.rs:913`).
- Verify caps signature candidates at `MAX_SIGNATURE_CANDIDATES = 8` (`crates/ocx_sign/src/verify/pipeline.rs:55`). It lists referrers unfiltered and filters on `artifactType` client-side (`pipeline.rs:418-440`).
- Copy caps referrers per leaf at `MAX_REFERRERS_PER_LEAF = 256` (`crates/ocx_oci/src/copy.rs:27`).

**One type.** `ocx_oci::blob_lookup::BlobLookup` reads plain directories in the blob layout. Every consumer (`ocx_index`, `ocx_sign`, `ocx_package_manager`, copy) reaches `ocx_oci`, and nothing in `ocx_oci` may reach `ocx_store`.

- **One path function.** `ocx_oci::blob_lookup::blob_data_path(root, registry, digest)` returns `<root>/<to_relaxed_slug(registry)>/<cas_shard_path(digest)>/data`. `BlobStore`, `BlobWriter` and `BlobLookup` all call it. `cas_shard_path` moves from `ocx_store` into `ocx_oci::cas_path`; the `ocx_store` copy is deleted.
- **Not a transport wrapper.** A wrapper sees the physical host after mirror routing, and blobs are keyed by the logical registry. Each seam passes the logical registry explicitly.

**Referrer marker grammar** lives in `ocx_oci::blob_lookup`, beside `blob_data_path`, and is used by the home `BlobStore` too.

- A marker is an empty file `referrers/<alg>-<hex>` in the subject's blob directory. Its name is the referrer manifest digest.
- **Filter before the cap.** A marker counts as a candidate only after its referrer manifest is read, digest-checked, names this subject in `subject`, and carries an `artifactType` verify accepts. Only then does `MAX_SIGNATURE_CANDIDATES` apply.
- **Per-source cap.** At most 256 markers are read per subject per root. The value is copy's `MAX_REFERRERS_PER_LEAF`; the constant moves to `ocx_oci::blob_lookup::MAX_REFERRERS_PER_SUBJECT` and copy uses it from there.
- **Markers never displace the live listing.** Under a live listing, candidates are the live listing first, in digest order, then marker-only candidates. A planted marker can add a candidate, never crowd one out.
- **`.complete`** distinguishes an empty listing from a missing one (Decision 1).

**Copy from an export.** Copy reads leaf manifests, blobs and referrers through the lookup first, keyed by the source identifier's registry as written. `copy.rs:382-407` re-hashes every streamed blob, so a lookup source is safe.

- **By digest:** ships. `ocx --blob-dir DIR/blobs package copy reg/repo@sha256:… -p linux/amd64 -i mirror/repo:1` needs no source registry for content.
- **By tag: ships.** Copy already resolves an index-served source tag through the index chain: `Publisher::copy` calls `Index::resolve_version`, which asks the local index for an index-served name (`crates/ocx_package/src/publisher/copy.rs:364-386`, `crates/ocx_index/src/lib.rs:346-368`). So `ocx --index DIR/index --blob-dir DIR/blobs package copy reg/repo:1 -i mirror/repo:1` works today for an index-served namespace. For a derived name `resolve_version` returns `Registry`, a live tag read. This ADR changes one rule: when the blob lookup is not empty, a derived name also resolves through the local index first, and live only on a miss. Reason: attaching a blob directory says the user is copying from local content.
- **Keep tags** are written at the target from the copied leaf digest (`publisher/copy.rs:239`); they need nothing from the source.
- **Description (`__ocx.desc`).** `--with-description` reads the description from the source registry, canonical and unrouted (`crates/ocx_cli/src/command/package_copy.rs:152-168`, `crates/ocx_package/src/publisher.rs:351-357`). An export carries no tags, so it cannot carry it. With the source unreachable that flag fails as today, after the package landed; `ocx package description push --from` copies it later.
- **Cosign sidecar tags** (`<alg>-<hex>.sig`, `.att`, `.sbom`) are HEADed by tag at the source (`copy.rs:236-306`). When the source registry cannot be reached and the leaf came from the lookup, the sweep is skipped with one warning naming the leaf. Reason: a sidecar tag is a tag, and an export holds none; the signatures that matter travel as OCI 1.1 referrers.

### Decision 6 — Check 4: path lists on Windows

**Evidence.** `OCX_CONFIG` is a `PathList` (`crates/ocx_env/src/vars.rs:56-65`). It is split with `std::env::split_paths` in `ocx_config/src/loader.rs:200`, `ocx_cli/src/app/context.rs:126` and `ocx_config/src/shell.rs:666`. On Windows `split_paths` uses `;` and honours `"` quoting. The outlier `ocx_config/src/env.rs:264` splits `PATHEXT`, an extension list, not a path list.

**Decision.** `OCX_BLOB_DIR` is a `PathList` read with `std::env::split_paths`, exactly as `OCX_CONFIG`. Empty entries are skipped, an empty value is unset, at most 32 entries. The `PATHEXT` outlier is correct as written.

### Decision 7 — Check 5: names

| Surface | Blob directories (read-only) | Blob cache (writable) |
|---|---|---|
| Flag (root, every command) | `--blob-dir <PATH>`, repeatable | `--blob-cache-dir <PATH>` |
| Environment | `OCX_BLOB_DIR` (`PathList`) | `OCX_BLOB_CACHE_DIR` (`Path`) |
| Config (`config.toml` tiers only) | `[blobs] dirs = [...]` | `[blobs] cache_dir = "..."` |
| Docs noun | blob directory | blob cache |

- IC-13: env = `OCX_` plus SCREAMING(long flag). `--config` / `OCX_CONFIG` is the precedent for a singular repeatable flag with a list variable.
- **IC-08, `dirs` against `dir`.** A key holding one path ends `_dir` (`cache_dir`). A key holding a list is a plural noun, as `[shell] profiles` and `[registries."<ns>"] trusted_hosts` are. `dirs` is the plural of the `_dir` family, so both rules hold.
- IC-12: each flag takes one value type.

**Which tiers may set `[blobs]`.** Every `config.toml` tier: system, user, `$OCX_HOME/config.toml` and the `[managed]` snapshot. A fleet may set a CI cache, and blob directories are digest-checked, so neither can change what a digest means.

- **Managed-tier paths must be absolute.** A relative path there would anchor at the snapshot file under `$OCX_HOME`, which no operator means. A relative managed path is refused at load (78).
- **Not `ocx.toml`.** A project file must not redirect a machine's reads or writes.
- **Not package metadata.** A package cannot set `OCX_BLOB_DIR` through its env: publish refuses any `OCX_*` key (`crates/ocx_package/src/metadata/validation.rs:186-193`).

**Precedence and merging.**
- Blob directories: the flag list if any flag is given, else `OCX_BLOB_DIR`, else the merged config. The winning tier replaces the others whole.
- Config tiers merge `dirs` the way every plain list merges today: a later tier replaces it.
- Cache: flag ▸ env ▸ config. An empty env value is unset.
- Relative paths: a config path anchors at its config file's directory (`FileReference::anchored_at`); a flag or env path joins onto the working directory.
- Both variables propagate to child ocx (`UserInput`), as `OCX_INDEX` does, so a launcher re-entry reads the same lookup.

**Missing directories.**
- A `--blob-dir` that does not exist is `blob_dir_not_found` (79). The user typed it.
- A blob directory from `OCX_BLOB_DIR` or config that does not exist warns once and is skipped when a read reaches it. Reason: an unmounted CI volume must not break `ocx clean`, `ocx config` or any command that reads no blob.
- A missing cache directory is created on first write.

**Commands.**

```
ocx package export -o <DIR> [-p <PLATFORM>]... [--referrers | --no-referrers] <PACKAGE>...
ocx package import <DIR> [-p <PLATFORM>]... [<PACKAGE>]...
ocx [--global | --project <PATH>] export -o <DIR> [-g <GROUP>]... [-p <PLATFORM>]... [--referrers | --no-referrers]
ocx [--global | --project <PATH>] import <DIR> [-g <GROUP>]... [-p <PLATFORM>]...
```

**Report roots.** `export` (shared by both export commands) and `import` (shared by both import commands). Fields are in system design § 9.

**New `error.detail` slugs.**

| Slug | Exit | Next action |
|---|---|---|
| `blob_dir_not_found` | 79 NotFound | Name a blob directory that exists |
| `export_not_found` | 79 NotFound | The export lacks the registry, package, tag, platform or blob named; re-export with it |
| `export_dir_invalid` | 65 DataError | Fix or re-create the export: no marker, unmapped directory, slug collision, catalog and root disagree |

Reused: `digest_mismatch` (65), `invalid_manifest` (65), `no_signatures_found` (79), `offline_mode` (81), `index_resolution_blocked` (81), usage (64), I/O (74). No new exit code; 79 and 65 already carry at least three slugs each.

### Decision 8 — Writer and cache protocol

**What the cache receives.** A network fetch writes every blob it fetched: metadata through `ChainedIndex`'s write-through, layer archives through a tee in the layer pipeline. A blob-directory hit is not copied into the cache. The home never keeps a layer archive.

**Tee point.** Between `.take(blob_total_size)` and `HashingAsyncReader::new` (`client.rs:921-923`). The whole reader chain is polled on a blocking thread: each decoder arm runs `tokio::task::spawn_blocking` over `SyncIoBridge::new(decoder)`, and the hasher sits inside the decoder (`client.rs:958-1006`). So the tee may write with `std::fs` on that thread. No channel and no `tokio::fs` is needed.

**`BlobWriter` protocol** (from the concurrency research):
1. If the target `data` exists, read and hash it. A verified occupant ends the write as `AlreadyPresent`.
2. If the occupant's sibling `digest` file names a different full digest, the shard path is shared by another blob. The write is refused: `export_dir_invalid` in an export, one warning and a skipped write in the cache. Another digest's entry is never overwritten.
3. Otherwise the occupant is corrupt. The write continues and replaces it.
4. Create a temp in `<root>/.tmp/` with the umask default mode, never a hardcoded one.
5. Stream into the temp and hash in the same pass. On a mismatch, delete the temp and fail.
6. `sync_data` the temp.
7. Refuse a symlinked ancestor below the root (`refuse_if_symlink_in_path`), then `create_dir_all` the parent. The check runs before every `create_dir_all`.
8. Publish: `persist_temp_file_noclobber` for a new entry; `persist_temp_file` (replace) for a corrupt occupant. On `Occupied`, re-verify the winner: verified is `AlreadyPresent`, corrupt goes back to step 3 once.
9. On Windows, retry the rename up to 2 s; success if the target appeared and verifies.

- **Remove never follows a link.** `BlobCacheDir::remove` checks the ancestors as in step 7, then removes the `data` file itself, never a link target.
- **Concurrency.** Several processes and CI jobs may share one cache now. No lock: every writer of a digest writes the same bytes.
- **Not guaranteed** in this release, tracked in [ocx-sh/ocx#760](https://github.com/ocx-sh/ocx/issues/760): concurrent eviction, network file systems, multi-user permissions, power-loss durability, single-fetch deduplication, cleanup of orphan temps, and the race between the ancestor check and the write.
- **Nothing prunes.** `ocx clean` never walks a cache or an export. Deleting a cache directory is always safe: every entry is a copy of registry content.

**Export commit point.** Re-export into an existing `DIR` must never leave a catalog that names a root it did not write.

- A per-`DIR` lock, `lock_scoped` under `$OCX_HOME/locks` keyed by `DIR`, serialises exports into one directory.
- Write order: blobs, dispatch objects, roots, then the catalog, then `ocx-export.json`. The catalog is the commit point; the marker is written last.
- Import checks each root against its catalog entry (`sha256(root bytes)`). A disagreement is `export_dir_invalid`, naming "re-run export".

**`BlobStore` stays as is** for the home, apart from calling `blob_data_path` and the marker functions. Export writes `DIR/blobs` through `BlobWriter`, plus the `digest` recovery file.

**Named exception.** `patch_discovery.rs:1129` reads a patch descriptor's layer from the home `BlobStore`. A descriptor layer is a small JSON payload, so "the home keeps no layer archives" excludes it by name.

### Decision 9 — Export behaviour

- **Lookup.** Export fills `DIR` through the lookup order. Layer archives never come from the home, so export downloads them unless a blob directory or the cache holds them. Offline with an archive nowhere local is `offline_mode` (81).
- **Bounded downloads.** Blob transfers per leaf are bounded by copy's `MAX_CONCURRENT_BLOB_TRANSFERS`. Packages fan out under the context's existing `--jobs` bound.
- **Platforms.** No `--platform` exports every platform of the image index; `--platform` is a repeatable filter. A digest identifier needs exactly one `--platform`, as `ocx package copy` rules (`command/package_copy.rs:97-104`).
- **Filtered exports.** The image index is copied verbatim; only the exported platforms' leaves, configs and layers are present. Exporting again with another `--platform` into the same `DIR` adds that platform.
- **Dependencies.** The dependency closure of every exported platform travels too, by digest, with no pins.
- **Referrers.** Carried by default (`--referrers` / `--no-referrers`). The listing is read through the lookup, then the network. Offline with no complete local listing and referrers on is `offline_mode`, naming `--no-referrers`.
- **Extraction.** Export extracts nothing and roots nothing.
- **Root `ocx export`.** Every entry of `ocx.lock`, or of `$OCX_HOME/ocx.lock` under `--global`, for the selected groups (`-g`, default every group) and every locked platform unless filtered. `--project <PATH>` selects the project as everywhere else. With an active patch snapshot beside the lock, its companions and descriptors travel too.

### Decision 10 — Import behaviour

**Import is a pull with the export attached, plus a pin update. Without names it only adds pins; a named package's pins may move.**

- **Never dials.** Import runs with `ChainMode::Offline` and no client.
- **Lookup order for import:** the home, then `DIR/blobs`, then the configured blob directories, then the cache. No network step.
- **Names narrow the set and give consent.** No names imports every package and tag the export lists. Names select a subset of that; they never add a tag. A named tag the export lacks is `export_not_found`. Naming a package is also explicit consent to move its pins.
- **Digest identifiers.** `ocx package import DIR reg/repo@sha256:… -p linux/amd64` materializes that digest from `DIR/blobs`, needs no `index/`, and writes no pin.

**The tag plan.** Per package, each exported tag is classified against the home:

| Home state for the tag | Package not named (no-names import) | Package named |
|---|---|---|
| Absent | Pin added after content: `added` | Pin added after content: `added` |
| Pinned to the same digest | Nothing written: `unchanged` | Nothing written: `unchanged` |
| Pinned to a different digest | Home pin kept, content not materialized, one warning: `conflict` | Pin moved after content, by the existing refresh rules: `replaced` |

- **Exit codes.** Import exits 0 when every package succeeded, `conflict` rows included. Reason: a moving tag such as `cmake:3` differs on every routine re-import, so a non-zero `conflict` would fail them all. An air-gapped user moves the pin by naming the package, the only route that home has.
- **Why naming is the consent.** It matches the owner-ratified rule that import writes pins only for the identifiers named (discussion decision 2). It also matches the security finding that moving a pin requires naming the package. A no-names import from a stale export therefore cannot roll a home back.
- **First sight never takes routing** from the export (Decision 3). Neither does a `replaced` tag: it moves a tag row only.

**Order: content first, pins last.**
1. Build the tag plan for every package from the export's sources. Nothing is written.
2. Materialize content for every `added`, `unchanged` and `replaced` tag, by `tag@digest` on the home's read-only index view (`LocalWritePolicy::ReadOnly`). A pinned-id pull writes no tag pointer, and `ReadOnly` writes no dispatch object. The selected platforms are the host by default, `--platform` repeatable. `conflict` tags materialize nothing.
3. Auto-verify runs inside each pull, as on every pull (Decision 12).
4. Commit pins for the package's `added` and `replaced` tags through `LocalIndex::import_tags`.

**Commit boundary.** One package. A package whose content or verification failed commits no pin. Every other package commits its own. Each root write is atomic per file through the existing writer. There is no transaction across packages, matching `ocx index update`. The exit is the first failure in input order, as there.

**Platforms.** Pins cover every platform, because a pin names an image index. An exported image index that lists a platform the export holds no blobs for:
- at import, a `--platform` naming it is `export_not_found` (79), naming the platform and the leaf digest;
- after import, an offline install for that platform is the existing `offline_mode` (81), naming the leaf it would fetch.

**What lands.** Metadata into the home `blobs/`, layers extracted into `layers/`, packages assembled. Layer archives stay in the export. Carried referrers go into the home `blobs/`, with markers, but only markers whose referrer manifest is present, digest-checked and names the subject. A `.complete` marker travels with them.

**No install, no roots.** No candidate symlink, no ledger entry. A later `ocx clean` collects the packages and their metadata blobs together. Afterwards `ocx package exec` and project tools pulled by digest run with the export deleted and the network blocked.

**No marker or no `index/`.** A package import of a directory with no `ocx-export.json` is `export_dir_invalid`. One with a marker but no `index/` (a root export) is `export_dir_invalid`, naming `ocx import` as the route for a lockfile.

**Provenance.** A blob-directory or cache hit writes no `refs/origins/` marker. Today `record_origin` runs whenever metadata was not provided (`pull.rs:448-449`, `:521-528`), and `consent::verified_sources` trusts that record (`consent.rs:186-229`). The record is gated on the leaf manifest having been fetched from the network in this pull. Reason: an export or a shared cache is not a resolution against the repository. A project whose tools came only from an import therefore needs an explicit `ocx shell allow` for a namespace grant.

**Trust.** Import trusts the exporter as an index source for the tags it adds, exactly as a configured index source is trusted. Digests bind content and the SSRF floor bounds dials, but nothing binds who chose the tag. For an export from an untrusted party, configure `[[trust.policy]]` for its registries, so every imported package must verify.

### Decision 11 — Root `ocx import`

**Root `ocx export` must be importable.** Its directory has a marker and `blobs/`, no `index/`.

- `ocx [--global | --project P] import <DIR> [-g GROUP]... [-p PLATFORM]...` is a project pull of every entry `ocx.lock` pins for the selected groups, with `DIR/blobs` attached and the network step removed.
- It registers the project in the `projects/` ledger, as `ocx pull` does, and renders the toolchain home the same way. Under `--global` it registers nothing: `$OCX_HOME/ocx.lock` is the implicit root.
- It requires `ocx-export.json` and cross-checks every lock entry's registry against it.
- It writes no index pin: lock resolution is index-free.
- With `patches.snapshot.json` beside the lock, companions and descriptors resolve from the snapshot pins and `DIR/blobs`, as compose already reads them snapshot-first.
- It reports the `import` root, one item per lock entry and platform.

**Reason.** A root export exists to recreate a project elsewhere. Reaching it through `ocx --offline --blob-dir DIR/blobs pull` works but skips the marker check and the lookup-only verification. A verb keeps the pair symmetric and puts both checks in one place.

### Decision 12 — Verification at import

- **The existing auto-verify runs.** Import is a pull, and auto-verify fires in `setup_impl` after resolve, before download (`crates/ocx_package_manager/src/tasks/auto_verify.rs:1-7`). It covers a package when a `[[trust.policy]]` or an index-declared publisher names it.
- **It must not dial.** Auto-verify holds a registry client "used even under `--offline`" and reads the artifact and its signature referrer through it (`auto_verify.rs:45`, `crates/ocx_cli/src/app/context.rs:35-38`). So import runs it with a `ReferrerSource::LookupOnly` seam: the subject manifest, the referrer listing and every referrer blob come from the home and the lookup, never the transport.
- **Sigstore offline.** Import passes `offline = true` for the trust services. A keyless signature then needs a local trust root and a pinned Rekor key, else the existing 81 (`auto_verify.rs:235`).
- **After `--no-referrers`.** A covered package has no candidates, so auto-verify fails with the existing `no_signatures_found` (79). When the export lacks `.complete` for the subject, the message says the referrers were not exported.
- **Uncovered packages** import unverified, as an uncovered pull installs unverified today.
- The previous draft's "verified marker" is deleted. Nothing records a verification.

### Decision 13 — Issue #759 rides as its own work package

**Question.** V7 needs `ocx exec` to keep project tools rooted. [ocx-sh/ocx#759](https://github.com/ocx-sh/ocx/issues/759) reports that it does not.

**Research.**
- The ledger is written by `ocx pull` (`render_toolchain.rs:464-465`), `ocx lock` (`ocx_project/src/lock.rs:707`) and mutations (`mutation.rs:179`). `app/project_context.rs` states that loaders never register.
- `ocx exec` composes through `compose_roots` (`toolchain_exec.rs:176-193`) and reaches no writer.
- `register_project_dir_best_effort` always rewrites the link through `replace_atomic`, even when it already exists (`crates/ocx_project/src/registry.rs:89-176`).
- The per-prompt hook composes with `Materialization::LocalOnly` (`crates/ocx_package_manager/src/activation.rs:456`), so it never fetches.

**Decision.**
- **Scope: `ocx exec` only.** `env`, `shell` and `direnv export` stay out of this work package and remain on the issue.
- **The per-prompt hook does not register.** It fetches nothing, so it creates no unrooted content. Registering there would root every consented project a user enters, and add a write to every prompt. Consent stamps under `state/projects/<key>/` are not GC roots and are unchanged.
- **Rule.** A project `ocx exec` registers the project when it is not yet in the ledger, whether or not anything was fetched. It never registers under `--global`.
- **No write when registered.** The work package adds `ProjectRegistry::register_if_absent` in `crates/ocx_project/src/registry.rs`: one `read_link`, and a write only when the entry is absent or points elsewhere. The work package owns that file.
- **Acceptance criteria**, written as failing tests before the fix:
  1. A project `ocx exec` on a fresh clone, then `ocx clean`: the tools survive.
  2. An `ocx exec` in an already registered project performs no write under `projects/`.
  3. `ocx package exec` outside any project leaves nothing in `projects/`.
- The fix lands as its own `fix:` commit referencing #759.

### Decision 14 — Containment, no new directory type

`ContainedDir` existed only for the withdrawn layout reader. It is not reintroduced.

- Index documents are read through `FileIndexTransport` and its caps. Repository names go through `ensure_repository_contained`.
- **Metadata reads** use `ocx_util::fs::read_bounded(path, cap)`, which refuses anything that is not a regular file.
- **Layer streams** open the way `FileIndexTransport` does: `O_NONBLOCK` on Unix, then a regular-file check on the open handle (`crates/ocx_index/src/file_transport.rs:92-174`). A FIFO or device swapped in after a path check cannot block or feed the reader.
- Blob `data` may be a symlink to a regular file: the digest makes the bytes safe (Git alternates precedent).
- Writes into `DIR` and into the cache refuse a symlinked ancestor below the root, checked before each `create_dir_all`.

## The five checks

1. **Import's index source:** an in-memory `OcxIndex` over `FileIndexTransport` per `DIR/index/<dir>/`, mapped to a registry only through `ocx-export.json` and cross-checked against named identifiers; kind and routing from the home; new `LocalIndex::import_tags` driven by a tag plan that moves a pin only for a named package.
2. **Partial catalog:** export resolves tags through the normal chain, writes roots restricted to exported tags with the existing writers, published shape always, catalog re-authored and written last under a per-`DIR` lock.
3. **Copy:** one `ocx_oci::blob_lookup::BlobLookup` keyed by logical registry with one `blob_data_path`; copy reads it by digest and by tag, the derived-name tag through the local index when a blob directory is attached.
4. **`PathList` on Windows:** `std::env::split_paths` as `OCX_CONFIG`; `env.rs:264` is `PATHEXT`, not a path list.
5. **Names:** `--blob-dir` / `OCX_BLOB_DIR` / `[blobs] dirs`; `--blob-cache-dir` / `OCX_BLOB_CACHE_DIR` / `[blobs] cache_dir`; commands `ocx package export`, `ocx package import`, root `ocx export`, root `ocx import`; marker `ocx-export.json`; roots `export`, `import`; slugs `blob_dir_not_found`, `export_not_found`, `export_dir_invalid`.

## Consequences

**Positive.**
- One directory serves offline transfer, CI caching and a later Bazel track.
- No translation layer: export writes the home's grammar with the home's writers.
- A no-names import never moves a pin, and a pin lands only after its content verified.
- Registries never share a blob, on disk or in lookup.

**Negative.**
- oras, skopeo and regctl cannot read an export.
- The relaxed slug is now part of a persisted layout; the marker is what keeps it reversible.
- A cache grows without bound until [ocx-sh/ocx#760](https://github.com/ocx-sh/ocx/issues/760) lands.
- An export discloses internal registry host names and repository paths, in the marker and in `index/`. The guide says so.
- A no-names import leaves a `conflict` tag on the home's digest and exits 0. A script that needs the exported digest must read the report or name the package.

**Neutral.**
- `cas_shard_path` and `INDEX_REFRESH_CONCURRENCY` move crates. Both are internal, so no shim is kept.

## Migration and rollout

- **No published export exists.** The withdrawn layout never shipped.
- **Existing homes and configs** are unaffected. With no blob directory and no cache, the lookup is empty and every path behaves as today.
- **Interface contract.** Every change is additive: four commands, two root flags, two variables, one config section, two report roots, three slugs. The counterparts move in the same commit: `cli.json`, reference regeneration, schema goldens, `CONTRACT` rows, `error.detail` docs. No ledger entry, since nothing breaks.
- **Smoke tier.** Root `export` and root `import` are new top-level verbs: a `@pytest.mark.smoke` test each, a `command` marker, and a raised `test/SUITE_FLOOR`.
- **Docs.** The guide states: deleting a cache is always safe; a cache must not be shared across a trust boundary that private-registry content would cross; an export discloses internal hosts and paths; an untrusted export needs `[[trust.policy]]`.
- **Changelog** is the commit subjects, per repository rule.

## Validation

Each acceptance test, its proof, and the mutation that must turn it red once.

| # | Acceptance test | Proof | Shown red by |
|---|---|---|---|
| V1 | Export from a non-default registry, then import into an empty home with the network blocked; install by tag works | `test_import_then_offline_install_by_tag` | Import skipping `import_tags` (no pin, install exits 79) |
| V2 | A changed byte is refused (exit 65) | `test_import_refuses_changed_byte`, network blocked | Blob-directory mismatch not remembered (exit becomes 81) |
| V3 | Imported for one package writes no pin for another | `test_import_one_name_pins_only_it` | Export copying the whole home root |
| V4 | A trust-policy package verifies offline from carried referrers, fails after `--no-referrers` | `test_import_verifies_from_carried_referrers` | Import not storing referrer markers (79 in the first half) |
| V5 | `oras cp` reads the export | **Dropped.** The owner accepted that oras cannot read the home layout. | — |
| V6 | Import, delete the export, block the network: `ocx package exec` runs | `test_import_then_exec_without_export` | Import leaving layers unextracted (exec exits 81) |
| V7 | Import a package export, delete it, run `ocx exec` in a project whose `ocx.lock` pins the imported digests, `ocx clean`, then `ocx exec` again offline | `test_import_then_project_exec_survives_clean` | Reverting the #759 registration (clean collects, exec exits 81) |
| V8 | Import installs nothing; `ocx clean` removes packages and metadata blobs together | `test_import_roots_nothing` | Import creating a candidate symlink |
| V9 | No names pins every tag; one name pins only it | `test_import_no_names_pins_every_tag` | Import enumerating one source only |
| V10 | `--platform` held in the export assembles; a missing one fails naming it | `test_import_platform_present_and_missing` | Error message without the platform |
| V11 | Registries `a` and `b` with the same repository path stay apart | `test_export_keeps_registries_apart` | Keying blobs by repository |
| V12 | Concurrent pulls into one empty cache | `test_concurrent_pulls_share_cache` (8 processes) | `persist` instead of `persist_noclobber` plus an unverified fast path |
| V13 | Warm cache: zero registry requests | `test_warm_cache_zero_registry_requests` | Tee skipping metadata |
| V14 | Export with no `--platform` holds all platforms; two `--platform` hold exactly two | `test_export_platform_filter` | Filter ignoring the second value |
| V15 | The interface gate passes | contract lint, compat gate, conformance hook | Removing a report golden |
| V16 | A no-names import keeps a home pin to another digest, reports `conflict`, exits 0; naming the package moves it, reports `replaced` | `test_import_conflict_and_named_replace` | `import_tags` moving a pin without a name (first half), or ignoring the name (second half) |
| V17 | Root export, root import, patch snapshot: composed env offline equals connected | `test_root_import_patch_snapshot_round_trip` | Root export skipping snapshot companions |
| V18 | A planted cache file writes no origin: `verified_sources` returns `None` | `pull::tests::lookup_hit_records_no_origin` | Dropping the network gate on `record_origin` |

Unit proofs sit beside each contract in system design § 13.

## Out of scope

- **Track 2** (Bazel consumption). It needs a blob found from a registry and a digest, `blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/data`, with no index lookup. Decision 1 provides exactly that.
- **A later `--format oci-layout` export**, one OCI layout per registry as in option A′, as the oras and skopeo interop route. It would sit beside this format, never replace it.
- **Cache eviction, shared volumes, network file systems, permissions:** [ocx-sh/ocx#760](https://github.com/ocx-sh/ocx/issues/760).
- **An inventory of an export's contents** beyond what `index/` lists.
- **Carrying cosign sidecar tags and `__ocx.desc`** in an export (Decision 5).
- **`ocx env`, `ocx shell`, `ocx direnv export` registration** (Decision 13; stays on #759).
- **A switch to disable a config-set cache per invocation.** Unset it in the environment or config instead.

## Open questions

None. Every question in the discussion and in review round 1 is decided above. No `[NEEDS CLARIFICATION]` marker remains.

## Changelog

| Date | Change |
|---|---|
| 2026-11-08 | First version: OCI image layout and a Bazel fetch plan. |
| 2026-11-23 | Full rework after the owner reopened Track 1 on 2026-11-22. Withdrawn: OCI image layout, `ref.name` grammar, the layout version annotation, `LayoutTransport`, the fetch-plan report and phase, `ContainedDir`. New: home-layout export, blob directories and blob cache, one lookup order, restricted pin snapshot, the five checks, the #759 work package. |
| 2026-11-23 | **Review round 1.** (1) Root marker `ocx-export.json` with `format_version` and the registry map, at the export and cache roots; import maps only through it; directory ladder deleted; slug-collision refusal; divergence from "no feature-specific format" recorded. (2) Import never moves a pin: tag plan, `conflict` status, exit 65; no routing from the export. (3) Content first, pins last; per-package commit boundary. (4) No origin marker on a lookup hit. (5) Mismatch policy: warn and continue, verdict 65, short local read 65, cache delete failure tolerated, discarded count on 81, clean layer retry. (6) Verification through the existing auto-verify with a lookup-only seam; "verified marker" deleted. (7) Referrer marker grammar in `ocx_oci`, shared `blob_data_path`, filter before cap, per-source cap, `.complete`. (8) Export resolves tags through the normal chain. (9) V7 restored verbatim. (10) Root `ocx import`; digest import. (11) Copy by tag ships. (12) Tee verified on a blocking thread. (13) Writer verifies occupants, shard collisions, export commit point. (14) Filtered exports defined. (15) #759 limited to `ocx exec`, hook excluded, no-write AC. (16) Missing env/config blob dirs warn. (17) Open-handle reads, symlink check per write. (18) Umask mode, config tiers, managed paths absolute. (19) Context reasons, isolation criterion, option A′, honest reversibility, `--format oci-layout` out of scope, Go prior art. (20) Spec fixes. (21) Origin debug log, bounded downloads, docs. |
| 2026-11-23 | **Re-validation.** (B1) `conflict` no longer exits 65: a no-names import adds pins only and reports `conflict` with exit 0; a named package's tags may move (`replaced`), with content materialized first; statuses `added | unchanged | replaced | conflict`; the copy precedent sentence dropped. (W1) The 65 verdict needs a bad blob-directory copy; a cache-only mismatch is a miss. (W2) End-of-sources branch for import and `--offline`. (W3) A blob absent everywhere at import is `export_not_found` (79). (W4) An unconfigured registry imports as `Derived`, verified in `chained_index.rs:196-207`. (W5) Export tag-resolution scenarios; offline unknown tag is `index_resolution_blocked` (81). (W6) `export` and `import` roots checked against IC-03 and IC-04. (S1-S3) Scenario wording; `blob_lookup::marker` renamed `export_marker`. |
