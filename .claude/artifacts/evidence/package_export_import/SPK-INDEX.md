# SPK-INDEX — evidence for the index import path

Pipeline SPK-INDEX (wave 0). Spikes SP-1, SP-2, SP-3, DV-2's third site, system design § 16 metadata-seam claim.
Method: code reads plus throwaway tests (two modules added inside `crates/ocx_index/src/{local_index,chained_index}.rs`,
run with `cargo test -p ocx_index --features __testing --lib -- spike_probe sp1_offline`, 3 passed, then deleted; tree clean).
Line numbers are at commit `c2db3902`.

## Verdicts

| ID | Verdict | A0 stub shape |
|---|---|---|
| SP-1 | Claim holds, design form fails, **fallback is reachable** | `LocalIndex::source_kind_for_import(&self, registry) -> SourceKind` over `is_index_namespace`; no `kind_by_registry` map |
| SP-2 | `refresh_published` takes rewritten bytes but **cannot take a tag set**; partial fallback applies | `import_tags` composes `persist_dispatch` + `commit_and_sweep_published_root(.., RootScope::Tags(set))` |
| SP-3 | Holds | no stub change |
| DV-2 third site | **Is on the import path** (not export, copy, verify) | lookup consulted in `ChainedIndex::fetch_manifest_raw_bytes` before the Offline early return |
| § 16 seam claim | **Fails as stated** (3 reads reach the store, 1 does not) | see table below |

## SP-1 — offline chain has no sources; kind from configured namespaces

- Claim (F6): offline, `kind_for_registry` returns `Derived`; configured namespaces are on `LocalIndex.index_namespaces`.
- Result: confirmed. `chained_index.rs:201-207` finds no source, returns `Derived`. `context.rs:739-740` (`None => (Offline, Vec::new())`) builds no sources when offline.
- Probe: a `LocalIndex::with_index_namespaces({REGISTRY})` under `ChainedIndex::new(local, vec![], ChainMode::Offline)`:
  `kind_for_registry(REGISTRY) == Derived` (green), `local.is_index_namespace(REGISTRY)` true, `is_index_namespace("other.io")` false.
- Reachability: `LocalIndex::is_index_namespace` is `pub` (`local_index.rs:101`); the CLI sets it from config even offline
  (`context.rs:362-364`, filter at `context.rs:1174-1187`, which mirrors `is_published_namespace`). `ChainedIndex` already reads it
  (`chained_index.rs:981`). So `kind_by_registry` on `ImportRequest` is not needed.
- Stub shape for A0:
  - `LocalIndex::source_kind_for_import(&self, registry: &str) -> SourceKind` = `Published` if `is_index_namespace(registry)` else `Derived`.
  - `Index::source_kind_for(&self, registry)` (C-007) delegates to the chain: a serving source's `source_kind()`, else that method. It must be
    used by import only. Do **not** change `ChainedIndex::kind_for_registry`: it also feeds `resolve_dispatch`, `physical_reference`,
    `list_*` (chained_index.rs:295, 314, 618, 650, 661, 723, 888, 904), and flipping offline `Derived` to `Published` there would add
    catalog cross-checks to every offline read.
- Caveat: `index_namespaces` excludes a namespace whose compiled-default index is suppressed by a local `[mirrors]` registry pin
  (`context.rs:1156-1167`); such a registry imports as `Derived`, which matches how it resolves.

## SP-2 — `refresh_published` over `FileIndexTransport`, restricted root

Probe: an export tree (`config.json`, `c/index.json`, `p/kitware/cmake.json` with tags 1,2,3 and a foreign `repository`,
`p/kitware/cmake/o/sha256/<hex>.json`), served by `OcxIndex` over `FileIndexTransport` with an empty stub OCI client (so no registry dial is possible).

| Check | Outcome |
|---|---|
| Source reads tag-lane dispatch objects through the file transport, no registry (`ocx_index.rs:1144-1157`, `:864-885` re-derives the digest) | holds: objects for the named tag land in the home `o/` |
| `refresh_published(tagged id, source, rewritten_bytes, root)`, first sight | home root `repository` = `oci://example.com/kitware/cmake`, tags = {1} only, tag 2 object absent |
| Same call with the **unrewritten** bytes (red) | home root `repository` = export's `oci://ghcr.io/ocx-contrib/cmake`: first sight adopts the fetched pointer (F8 confirmed) |
| Catalog check | `refresh_published` never reads `c/index.json` (code read, `local_index.rs:158-256`); `OcxIndex::fetch_catalog_strict` (pub, `ocx_index.rs:994`) returns `BTreeMap<repo, "sha256:<hex>">`, equal to `IndexStore::root_catalog_entry(root_bytes)` in the probe. `import_tags` must make this check itself, on the **exported** bytes, before rewriting |
| Tag set `{1,3}` via `refresh_published` | impossible: `in_scope` is one tag (`identifier.tag()`) or all (`RootScope::Package`, which migrates `repository`) |
| Fallback: `persist_dispatch` for 1 and 3, then `commit_and_sweep_published_root(id, rewritten, Tags(&["1","3"]))` over a home root that has tag 2 pinned elsewhere under a mirror pointer | tags {1,2,3}; `repository` stays the mirror pointer; tag 2 content untouched (conflict row kept); catalog entry in the home equals the sha256 of the committed bytes |
| Replace: `Tags(&["2"])` | tag 2 moves to the export digest; tags 1 and 3 survive (`merge_root` never deletes a durable tag; the ephemeral drop is limited to in-scope tags) |
| First sight with an empty set | no write (`merge_root` returns `None`, `local_index.rs:838-856`); no root created |

- Result: `refresh_published` accepts rewritten bytes (its `bytes` parameter is independent of `source` and `root`), but its scope is one tag or the package. The plan's SP-2
  fallback applies in its tag-set form, and its "rewrite on first sight" half needs no new merge code.
- Stub shape for A0 (`LocalIndex::import_tags`, `Published` arm):
  1. `source.fetch_catalog_strict()` (on the `OcxIndex`, before it is wrapped in `Index`) and `revalidate_root_document`; the entry must equal `IndexStore::root_catalog_entry(exported_bytes)`, else `export_dir_invalid`.
  2. `committed = read_root_document_bytes`; if `None`, rewrite `repository` to `oci://<registry>/<repo>` in a `serde_json::Value` copy; if `Some`, pass the bytes unchanged (Tags scope keeps the committed pointer).
  3. For each tag in `plan.added ∪ plan.replaced`: `persist_dispatch(source, id.clone_with_tag(tag))`. An `Ok(None)` must be an error, not a skip (`refresh_published` silently treats it as success at `local_index.rs:199`).
  4. `commit_and_sweep_published_root(id, &bytes, RootScope::Tags(&set))`.
  Both helpers are `pub(super)`/`pub` inside `local_index.rs`, so `import_tags` stays in that module.
- `refresh_published` itself needs no change; do not refactor it for import.

## SP-3 — identity pointer vs `guard_physical_dial`

- Result: holds. Both guards return `Ok` when `physical.registry() == logical.registry()`: `lib.rs:453` (`Index::guard_physical_dial`) and `chained_index.rs:238-240` (`guard_local_physical`).
- Probe: after the SP-2 commit, `LocalIndex::physical_reference(tagged, Published)` returns an identifier with registry `example.com` (equal to the logical one), and `Index::guard_physical_dial` returns `Ok`.
  Red: `oci://127.0.0.1/kitware/cmake` is refused (`Err`); the foreign-pointer home reports registry `ghcr.io`, so only a rewritten pointer short-circuits.
- Caveat for A0: the comparison is string equality on the host token as written, so the identity pointer must be built from the same `registry` string the identifier carries (`parse_repository_pointer` keeps host and port verbatim, `oci_identifier.rs:41-56`).

## DV-2 third site — callers of `fetch_manifest_raw_bytes`

`ChainedIndex::fetch_manifest_raw_bytes` (`chained_index.rs:832-851`) returns `Ok(None)` when `mode == Offline`, otherwise asks sources; it never reads the content store.

| Caller | Path | On export / copy / verify / import? |
|---|---|---|
| `local_index.rs:374` `persist_dispatch`, `:389` `fetch_dispatch_only` | online source walk (`chained_index.rs:446-448`), `refresh_*`, `index_common.rs:119` | no (sources only, never offline) |
| `ocx_package_manager/src/tasks/common.rs:327` `stage_chain_blobs` (`ChainRole::Index` and `Manifest`) | via `stage_and_link_chain_blobs`: `pull.rs:262, 459, 489, 634`; also `prepare_lazy.rs:46`, `render_toolchain.rs:168` | **import**: its pull runs on the offline view; offline the call returns `None` and the blob is skipped, leaving a dangling ref (doc comment at `common.rs:344`) |
| `common.rs:578` `stage_leaf_manifest` | `common.rs:789`, `prepare_lazy.rs:47`, `render_toolchain.rs:169` | same as above |

- Export does not use `Index::fetch_manifest_raw_bytes` for its blob fill (own `BlobWriter` path, C-010); copy and verify read through `ocx_oci::Client` and `ocx_sign` directly (`fetch_manifest_raw_bytes_addressed` in `ocx_announce`/`ocx_config` is the Client method, not this one).
- Result: DV-2 must include this site. Stub: `ChainedIndex::fetch_manifest_raw_bytes` consults `lookup.read_metadata` for a digest-addressed identifier **before** the `Offline` return and writes a hit to the home; tag-addressed identifiers are unaffected (lookup is digest-keyed).

## § 16 claim — every metadata read goes through the content-store seam

Fails as stated. `ChainedIndex` read sites:

| Site | Reads | Reached by a lookup attached at `fetch_blob` / `recover_absent_dispatch`? |
|---|---|---|
| `fetch_blob` (`chained_index.rs:771-826`) | config blobs, digest-addressed | yes: `content_store.read_blob` at `:774` |
| `recover_absent_dispatch` (`:330-367`) | index and leaf manifests behind an absent `o/` object; called from `fetch_manifest` (`:672`) and `fetch_manifest_digest` (`:739`) | yes |
| `fetch_manifest` / `fetch_manifest_digest` local branch | `resolve_dispatch` reads `o/` only | n/a (reaches the seam through `recover_absent_dispatch`) |
| `fetch_manifest_raw_bytes` (`:832`) | raw manifest bytes for staging | **no** (DV-2 third site) |
| `fetch_and_persist_chain` (`:430-480`) | source reads; writes leaf manifests to the content store (`:454`) | no read of the lookup; online only, not needed |
| `physical_reference`, `list_tags`, `list_repositories`, `declared_publishers` | roots in the local index | not blob metadata (index import writes these) |

`ocx_package_manager` metadata reads: config blob `common.rs:126` (`index.fetch_blob`, seam), `common.rs:314` (`fetch_blob`, seam), `common.rs:264` and `composer.rs:1256`, `patch_discovery.rs:1099/1129`
(`fs.blobs.read_blob` on the **home** store only: a lookup attached to `ChainedIndex` does not reach them; they run after staging, so the lookup must have written the hit to the home first, which C-007 requires),
and `common.rs:327/578` (above). Net: attach the lookup at three places, not two: `fetch_blob`, `recover_absent_dispatch`, `fetch_manifest_raw_bytes`.
Residual risk: any home-only `read_blob` that runs on a blob never staged (e.g. `patch_discovery`) will miss a lookup-only blob; patches are out of the export scope in the design, so this is noted, not fixed.
