# Discovery: Codebase Map for Public Index + Registry Indirection

Compiled from 4 parallel explorer reports (2026-07-12). Input for design phase of
`adr_public_index_registry_indirection.md` (D1–D15). Facts verified against live code.

## 1. Identifier flow (current, end-to-end)

```
CLI arg string
  → options::Identifier{raw}                    crates/ocx_cli/src/options/identifier.rs
    → with_domain(context.default_registry())
      → oci::Identifier::parse_with_default_registry(raw, default)
        crates/ocx_lib/src/oci/identifier.rs:78  ← registry becomes part of Identifier HERE
  → PackageManager / Index / FileStructure — all keyed on identifier.registry() verbatim
    → Client::transport_reference/transport_registry — MirrorMap rewrite (transport ONLY)
    → BlobStore/LayerStore/PackageStore/SymlinkStore — slugify(identifier.registry()) keying
  → ProjectLock — Identifier persisted bare (registry/repo) in ocx.lock
```

- `Identifier { registry, repository, tag, digest }` (`oci/identifier.rs:28`); `parse()` rejects
  bare inputs (`MissingRegistry`); `parse_with_default_registry` = CLI entry point.
- `canonical_reference()` is `pub(crate)`, push-path only, deliberately no public
  `From<&Identifier>` — existing "no bypass" enforcement precedent.
- `PinnedIdentifier` (digest-required wrapper) for resolve.json/lock/blob addressing.
- **No dual-name (requested/physical) type exists** — exactly one String per identifier today.

## 2. Rewrite seam + bypass call sites

**Insertion point (D8/D11 confirmed):** immediately after `options::Identifier::with_domain`
produces `oci::Identifier`, before PackageManager/FileStructure/Client. ~20 CLI command files
funnel through `transform_all`/`transform_optional` → one seam covers all.

**Bypasses (construct `oci::Identifier` directly, must be routed or exempted):**
1. `crates/ocx_lib/src/project/compose.rs` — ocx.toml tool bindings → identifiers for env composition
2. `crates/ocx_lib/src/config/managed.rs` — `[managed]` source identifier
3. `crates/ocx_lib/src/setup.rs` — hardcoded `ocx_cli_identifier()` = `Identifier::new_registry("ocx/cli", OCX_SH_REGISTRY)` (`identifier.rs:215`) — self-management fixed ref; rewrite must special-case or exempt
4. `project/lock.rs` — reads Identifier straight from lock TOML. **Intended** bypass per D11:
   locked refs are already-physical, no recursive alias — rewrite is a write-time concern.

**IndexImpl has no seam for name resolution** — its 4 methods all take resolved Identifier.
Confirms D8 rejection of RedirectingIndex: resolution must precede `Index` entirely.

## 3. Mirror precedent (transport-only rewrite)

- `MirrorMap::rewrite_repository(registry, repo)` (`oci/client/mirror_map.rs:67`) —
  HashMap<host, ParsedMirror>; Some((mirror_host, prefix/repo)) or None=identity.
- Applied ONLY at `Client::transport_reference` (:116) / `transport_registry` (:144) —
  every read routes through; push paths use `canonical_reference()` (mirror-free, read-only mirrors).
- Never touches storage keying, lockfile, or Identifier itself.
- Config: `[mirrors."<host>"] url = "..."` → `resolve_mirror_map()` (`config/mirror.rs:238`)
  merges config + `OCX_MIRRORS` env (env wins per-host), validates once at Context::try_init.

## 4. Storage keying (all `slugify(identifier.registry())`)

| Store | Path | File |
|---|---|---|
| blobs | `blobs/{slug}/{algo}/{2hex}/{30hex}/` | file_structure/blob_store.rs:64 |
| layers | same shape under `layers/` | file_structure/layer_store.rs:64 |
| packages | `packages/{slug}/{digest}` (repo NOT in path — cross-repo dedup) | file_structure/package_store.rs:154 |
| tags | `tags/{slug}/{repo_path}.json` | file_structure/tag_store.rs:40,56 |
| symlinks | `symlinks/{slug}/{repo_path}/{current,candidates/{tag}}` | file_structure/symlink_store.rs:55,98 |

Physical-registry keying (D11) holds automatically IF rewrite happens before these see the Identifier.

## 5. Lockfile + project file

- `ocx.lock` V2 (`project/lock.rs`): `LockedToolV2 { name, group, repository: Identifier (bare
  registry/repo), platforms: BTreeMap<platform, digest> }`. **Registry string persisted verbatim**
  — rewrite must normalize before `ocx lock` writes, else lock encodes logical name forever.
  Metadata: `lock_version: 2`, `declaration_hash` (RFC 8785 JCS over tools+groups only).
- `ocx.toml`: `[tools] name = "registry/repo[:tag][@digest]"`, `[group.*]`,
  `[package."registry/repo[:tag]"]` policy sections (NOT in declaration hash).
- Patch pinning: `patches.snapshot.json` beside ocx.lock (`ocx patch freeze`), separate file.

## 6. Config system

Tier order (low→high): `/etc/ocx/config.toml` (system, per-field lock) → user config-dir →
`$OCX_HOME/config.toml` → managed snapshot (identity-gated) → `OCX_CONFIG` env → `--config`.
Loader: `config/loader.rs:83-94`. Root `Config` (config.rs:26-76), **no deny_unknown_fields**
(forward-compat). Five sections: `registry` (RegistryDefaults), `registries`
(HashMap<String, RegistryConfig>), `mirrors`, `patches`, `managed`.

- **`[registries.<name>]` table EXISTS**: `RegistryConfig { url: Option<String>, system_locked }`
  (`config/registry.rs:19`, deny_unknown_fields). Today only consumer =
  `Config::resolved_default_registry()` (config.rs:163) resolving `[registry] default = "name"`;
  sole call site `context.rs:209-215`. NOT a general rewrite today.
- D10 lands as new field shape on this struct (single `url` typed by `index+`/`oci+` prefix).
  Needs: prefix parsing (mirror `MirrorConfig::parse_url` config/mirror.rs:165 pattern),
  `resolve_*` fn (mirror `resolve_mirror_map`), system-lock merge pattern (proven 3× —
  table-driven test config.rs:537-616 covers all lockable sections).
- `OCX_DEFAULT_REGISTRY`: read once at `Context::try_init` (context.rs:209); default constant
  `ocx_lib::oci::DEFAULT_REGISTRY = "ocx.sh"`; 40+ `context.default_registry()` call sites.
- Managed config: ManagedConfig forward-compatible (no deny_unknown_fields) — new `[registries]`
  fields distributable via managed tier for fleets.
- Schemas: schemars derive already on Config/RegistryConfig/etc → `task schema:generate` →
  `website/src/public/schemas/config/v1.json` auto-includes new fields.

## 7. Index abstractions

- `oci::Index` facade → `Box<dyn IndexImpl>`; impls: `ChainedIndex` (local cache + ordered
  sources + `ChainMode` {Default,Remote,Offline,Frozen} + singleflight), `LocalIndex`
  (tags.json per repo + content-addressed manifest blobs), `RemoteIndex` (Client + mem cache).
- Local index writes: exactly 3 entry points (`refresh_tags` = `ocx index update` only;
  `persist_manifest_chain` + `commit_tag` = chain-walk on Resolve miss). Structural test enforces
  Query-never-writes.
- Policy: Offline/Frozen unpinned-tag miss → `PolicyResolutionBlocked` exit 81.
- `ocx index` CLI: `catalog [--tags]`, `list`, `update [--check]`. `catalog` →
  `Client::list_repositories` → `/v2/_catalog` — **the dead end this ADR replaces**.
- **No non-OCI HTTP client exists** — sparse index client is a new surface. Reusable pieces:
  auth fallback (`auth::Auth::get_or_fallback`), progress callback type, per-host plain-HTTP flag.

## 8. Display / logging surfaces (dual-name D9 touchpoints)

- Single `Display` impl `registry/repo[:tag][@digest]` (identifier.rs:264); custom Serialize
  emits same string.
- `api/data/install.rs`: `InstallEntry { identifier, metadata, path }`; JSON report keyed by
  **raw input string** (requested form preserved there today).
- ~15 `log::*!` sites interpolating single Identifier (client.rs:201,211,220,490-494,546,733-736;
  package_manager/tasks/pull.rs:217, find.rs:39,42).
- D9 `resolved_from` field = addition to InstallEntry + sibling api/data structs (Printable pattern
  per subsystem-cli-api.md).

## 9. Flags

`--remote`/`--offline`/`--frozen` (+ env `OCX_REMOTE`/`OCX_OFFLINE`/`OCX_FROZEN`) → ChainMode
routing in chained_index.rs. Update family: `suppress_tag_commit`, no Default arm.

## 10. Prior art in .claude/artifacts (design inputs)

- `adr_public_index_registry_indirection.md` — THE spec (D1–D15)
- `adr_custom_oci_identifier.md` — Identifier type decisions
- `adr_index_routing_semantics.md` + `feedback_index_routing_semantics.md` — ChainMode routing
- `adr_oci_registry_mirror.md` + `research_registry_mirror.md` — mirror rewrite (Q5: read-only)
- `adr_managed_config_tier.md` — config tier + fleet distribution
- `research_phase21_identifier_pinned_resolve.md` — pinned resolution
- `plan_per_platform_lock_pinning.md` — lock V2 shape
