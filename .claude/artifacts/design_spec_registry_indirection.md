# Design Spec: Public Index + Registry Indirection (ocx.rs)

**Status:** Draft (design level — ready for `/swarm-plan` decomposition)
**Date:** 2026-07-12
**Author:** Architect worker (Claude) from Michael's task brief
**Refines:** `adr_public_index_registry_indirection.md` (D1–D15 — accepted, not re-litigated here)
**Inputs:** `research_index_indirection_discovery.md`, `research_sparse_index_formats.md`,
`research_index_announce_bots.md`, `research_ghcr_constraints.md`
**Subsystem rules honored:** `subsystem-oci.md`, `subsystem-package-manager.md`,
`subsystem-cli.md`, `subsystem-cli-api.md`, `quality-rust-errors.md`,
`quality-rust-exit_codes.md`, `arch-principles.md`

---

## 0. Scope and Non-Goals

**In scope.** The client-side machinery that turns a *logical* identifier
(`ocx.rs/kitware/cmake:3`) into a *physical* one (`ghcr.io/ocx-contrib/cmake:3`)
exactly once, early, before the existing pipeline (D8/D11); the `[registries.<name>]`
config typing (D10); a sparse-index HTTP client with conditional-GET freshness (D12/D13);
dual-name reporting + lockfile fidelity (D9); and the `ocx-sh/index` repo's layout,
render/validate CI, announce transport, and reconcile cron (D1/D4/D5).

**Out of scope (deferred per ADR, do not build here).** Signing/provenance/index-level
digest pinning; full yank *CLI semantics* (resolve-vs-install-vs-lock behavior — this spec
implements only the minimal "resolution refused" leaf); `all.json` search snapshot;
`ocx search`; sparse-index sharding; OIDC trusted-publishing announce (v2); the
server-side OCI facade (rejected in D8).

**Boring-tech posture (per `quality-core.md`).** Every new mechanism reuses an in-tree
pattern: config typing clones the `resolve_mirror_map` / `ParsedMirror` shape; the HTTP
client reuses `reqwest` (already compiled in via the `oci-client` fork) behind an
`OciTransport`-style trait seam; the on-disk cache reuses the `StateStore` directory
convention; freshness reuses `ChainMode`. Zero new innovation tokens spent.

---

## 1. Architecture at a Glance

```
CLI arg "cmake:3"
  → options::Identifier{raw}                        (unchanged)
  → with_domain(default_registry = "ocx.rs")        (logical) — resolved_default_registry now returns the KEY
      → oci::Identifier { registry:"ocx.rs", repository:"cmake", tag:"3" }
  ── NEW EARLY REWRITE STAGE (RegistryResolver) ─────────────────────────────
  → resolver.resolve(id)
      registry "ocx.rs" ∈ known-registries keys?  yes, type = Index
        → sparse-index GET https://index.ocx.rs/p/…/cmake.json  (ETag/304, state cache)
        → physical oci::Identifier { registry:"ghcr.io", repository:"ocx-contrib/cmake", tag:"3" }
      → ResolvedIdentifier { requested: <logical>, physical: <ghcr> }
  ───────────────────────────────────────────────────────────────────────────
  → PackageManager / Index / FileStructure  keyed on .physical()          (unchanged)
      → Client::transport_reference  → MirrorMap rewrite (ghcr.io → corp mirror)   (unchanged)
      → stores keyed slugify(physical.registry())                                   (unchanged)
  → ocx.lock  records requested (logical) + physical (repository) + per-platform digest
  → report/api  displays .requested(); physical in verbose / errors / resolved_from field
```

**Resolution order invariant (D11), preserved for free:** `logical → index resolve →
physical → mirror_map → actual host`. Index resolve happens at the CLI/lib seam (before
`PackageManager`); `mirror_map` still happens inside `Client`. Nothing about the mirror
stage changes.

**New module.** All new client-side runtime code lands in a new subsystem module
`crates/ocx_lib/src/registry_index/` (parsed-endpoint types stay in `config/registry.rs`,
mirroring how `ParsedMirror` lives in `config/mirror.rs` but `MirrorMap` lives in
`oci/client/mirror_map.rs`). A new `subsystem-registry-index.md` rule is a Phase-6
deliverable.

```
crates/ocx_lib/src/registry_index/
  mod.rs                 // re-exports; RegistryResolver
  endpoint.rs            // RegistryEndpoint, RegistryMap, default_registry_map()
  resolver.rs            // RegistryResolver + resolve()/resolve_all()
  resolved.rs            // ResolvedIdentifier (dual-name carrier)
  client.rs              // SparseIndexClient + SparseIndexTransport trait + reqwest impl + stub
  entry.rs               // IndexEntry (schema v1), IndexConfig (config.json), IndexOwner, EntryStatus
  cache.rs              // on-disk state cache (envelope: etag + entry), $OCX_HOME/state/registry-index/
  error.rs               // ResolveError (+ SparseIndexError)
```

---

## 2. Component Contracts

Every contract below is phrased as a testable behavior. Contract IDs (`C-xx`) are the
handles for the task-decomposition test matrix in §8.

### 2(a) `[registries.<name>]` config extension (D10)

The struct `RegistryConfig { url: Option<String>, system_locked }` already exists
(`config/registry.rs`), already has `deny_unknown_fields`, and already has the
system-lock `merge`/`lock_as_system` pattern (proven identical to `MirrorConfig`). This
spec adds **typed parsing** of `url` and a **resolver** — no struct field changes.

#### Parsed endpoint type (new, in `config/registry.rs`)

```rust
/// A parsed `[registries.<name>].url`, typed by its scheme prefix (D10).
/// Mirrors `ParsedMirror` — produced in the config layer so the
/// `registry_index` layer receives an already-parsed value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryEndpoint {
    /// `index+https://…` — resolve logical → physical via the sparse index at
    /// this endpoint URL (scheme + host + optional base path, no trailing `/`).
    Index { endpoint: String },
    /// `oci+https://<host>` — swap the identifier's registry component to this
    /// host directly; no index lookup. `scheme` retained for the plain-HTTP gate.
    Oci { scheme: String, host: String },
}

impl RegistryConfig {
    /// Parse `url` into a typed [`RegistryEndpoint`].
    ///
    /// # Errors
    /// - [`RegistryConfigError::MissingUrl`] — `url` absent or empty.
    /// - [`RegistryConfigError::MissingPrefix`] — no `index+` / `oci+` scheme prefix.
    /// - [`RegistryConfigError::MissingHost`] — `oci+` form with no host.
    pub fn parse_url(&self) -> Result<RegistryEndpoint, RegistryConfigError>;
}
```

- **C-01** `url = "index+https://index.ocx.rs"` → `Index { endpoint: "https://index.ocx.rs" }`.
- **C-02** `url = "oci+https://ghcr.io"` → `Oci { scheme: "https", host: "ghcr.io" }`.
- **C-03** `url = "oci+http://localhost:5000"` → `Oci { scheme: "http", host: "localhost:5000" }`.
- **C-04** `url = "ghcr.io"` (bare, no prefix) → `Err(MissingPrefix)`. *Pre-1.0 breaking
  change to the currently-unreleased untyped `url` — no migration shim (project memory:
  no backward-compat for unreleased features). The migration republish re-writes any
  local test configs.*
- **C-05** `url = None` / `""` → `Err(MissingUrl)`.
- **C-06** `url = "index+https://"` → `Err(MissingHost)`. `url = "oci+https://"` → `Err(MissingHost)`.
- **C-07** Unknown top-level field inside `[registries.<name>]` (e.g. `urll`) → deser
  error (existing `deny_unknown_fields`; regression-lock it).
- **C-08** Trailing `/` on an `index+` endpoint is trimmed exactly once (reuse the
  `strip_suffix('/')` single-strip rule proven in `MirrorConfig::parse_url`).

#### Resolver (new, in `config/registry.rs`, mirroring `resolve_mirror_map`)

```rust
/// Output of [`resolve_registry_map`]: the parsed `(name, RegistryEndpoint)`
/// entries (for the `RegistryMap`) and the `(name, url)` pairs (for forwarding
/// via `OcxConfigView`).
pub type ResolvedRegistries = (Vec<(String, RegistryEndpoint)>, Vec<(String, String)>);

/// Resolve the known-registries map: baked defaults (base) → `[registries]`
/// config → inherited `OCX_REGISTRIES` env (env wins per-name key), then parse +
/// validate every entry once. Owns the plain-HTTP gate for `oci+http://` endpoints
/// (CWE-319, same rule as mirrors). System-lock is honored by the existing
/// per-entry `RegistryConfig::merge`.
pub fn resolve_registry_map(
    config: &crate::config::Config,
    env_pairs: Vec<(String, String)>,
    insecure_hosts: &[String],
) -> Result<ResolvedRegistries, RegistryConfigError>;
```

- **C-09** Baked default map is the merge base: `resolve_registry_map(empty_config, [], [])`
  contains `ocx.rs → Index{https://index.ocx.rs}` and `ocx.sh → Index{https://index.ocx.rs}`.
- **C-10** A config `[registries.ocx.rs]` entry overrides the baked default for `ocx.rs`
  (config tier > baked base); a config-only name survives; env `OCX_REGISTRIES` wins
  per-name over config (one-way-door precedence — identical contract to `env_mirrors_win_over_config_per_host_key`).
- **C-11** A system-locked `[registries.ocx.rs]` entry ignores a lower-tier override
  (existing `merge` behavior — regression-lock through the resolver).
- **C-12** `oci+http://` endpoint whose host is absent from `OCX_INSECURE_REGISTRIES` →
  `Err(PlainHttpEndpointNotAllowed)` naming the endpoint host and `OCX_INSECURE_REGISTRIES`
  (mirror-gate parallel). `index+http://` is likewise gated.
- **C-13** `resolve_registry_map` errors re-wrap into `anyhow` at the `Context::try_init`
  boundary exactly like `resolve_mirror_map` does today.

#### `resolved_default_registry` redefinition (D10 interaction)

The existing `Config::resolved_default_registry()` dereferences `[registry] default =
"name"` through `[registries.name].url` and returns the *URL host*. Under D10 that host
is now a typed endpoint, and the rewrite stage — not this function — owns dereferencing.

**Chosen behavior (see §5 Trade-off T1):** `resolved_default_registry` returns the
**key name** (the logical registry) whenever `[registry] default` names a `[registries]`
entry, so bare names expand to a logical registry the rewrite then resolves. It returns
the name literally when no entry matches (unchanged v1 fallback), and `None` when no
default is configured.

- **C-14** `[registry] default = "ocx.rs"`, `[registries.ocx.rs] url = "index+https://…"`
  → `resolved_default_registry()` returns `"ocx.rs"` (the key), **not** the endpoint host.
- **C-15** `[registry] default = "corp"`, `[registries.corp] url = "oci+https://reg.corp"`
  → returns `"corp"` (the key); the rewrite later swaps `corp/x` → `reg.corp/x`.
- **C-16** `[registry] default = "reg.corp.example"` with no matching entry → returns
  `"reg.corp.example"` literally (bare-host default, no rewrite fires since it is not a key).
- **C-17** System-lock indirection guard (existing CWE-15 protection) is preserved: a
  system-locked `[registry]` whose target `[registries]` entry is unlocked still resolves
  to the name (not a lower-tier-injected endpoint) — regression-lock.

### 2(b) Early rewrite stage (D8/D11)

#### Placement

The transform seam is `options::Identifier::with_domain` → produces the *logical*
`oci::Identifier`. The rewrite is a **new async pass immediately after** it, owned by a
lib type and invoked from the CLI via a `Context` method (lib hosts substance, CLI thin —
project memory). `transform_all` stays synchronous and logical; a new
`Context::resolve_identifiers` runs the async rewrite:

```rust
// crates/ocx_cli/src/app/context.rs
impl Context {
    /// Resolve logical identifiers to physical via the registry map + sparse
    /// index (the early rewrite stage). One network round-trip per distinct
    /// index-typed name on a cold cache; 304 or state-cache hit otherwise.
    /// Order-preserving (input order), fan-out parallel per `subsystem-package-manager.md`.
    pub async fn resolve_identifiers(
        &self,
        logical: Vec<oci::Identifier>,
    ) -> Vec<Result<registry_index::ResolvedIdentifier, registry_index::ResolveError>>;
    // per-identifier outcomes, input order — partial-batch semantics per C-20b (§11.3)

    pub fn registry_resolver(&self) -> &registry_index::RegistryResolver;
}
```

Command pattern becomes: `transform_all` (logical) → `context.resolve_identifiers` →
manager task on `.physical()` → report using `.requested()`.

- **C-18** A command given `ocx.rs/cmake:3` calls the resolver before any `PackageManager`
  task; a stub transport asserting a single GET to `p/…/cmake.json` proves the ordering.
- **C-19** `resolve_identifiers([])` → `Ok([])`, zero transport calls.
- **C-20** Order preservation: `resolve_identifiers([a,b,c])` returns physical results in
  input order (spawn-with-index/sort pattern), even under parallel fan-out.

#### Bypass-site routing (the four sites from discovery §2)

| Site | Decision | Rationale (testable) |
|---|---|---|
| `project/lock.rs` (lock read) | **Exempt** | Locked `repository` is already physical (D11). Rewrite is a write-time concern. **C-21**: a locked install performs zero index GETs. |
| `project/compose.rs` | **Exempt** | Group entries come from the lock (physical, digest-pinned); positional `ToolSource::Explicit` identifiers are already transformed at the CLI seam before reaching compose. **C-22**: `compose_tool_set` performs no rewrite and its inputs are physical. *Guard:* the `run`/`exec` CLI commands must call `resolve_identifiers` on positionals **before** `compose`. |
| `config/managed.rs` | **Routed, pre-managed map** | The managed source identifier resolves through the registry map built from tiers *below* managed (baked + system + user + home + env), never the managed tier's own `[registries]` (chicken-and-egg: managed config may distribute `[registries]`). **C-23**: a managed source `ocx.rs/acme/config` index-resolves using the pre-managed map; a managed-tier-injected `[registries]` entry does **not** affect resolution of the managed source itself. |
| `setup.rs` `ocx_cli_identifier()` | **Exempt — baked physical constant** *(revised in review round 1; supersedes the earlier "Routed" draft)* | The "routed" draft assumed a single call site; review found the helper is **sync + pure with 13 sync call sites** (`setup/bootstrap.rs` ×7, `package_manager/tasks/update_check.rs` ×3, `command/self_group/activate.rs:116` — runs on every shell start, `setup.rs:722`, `setup/version_spec.rs`). Routing would (a) make async viral through lib, (b) couple shell startup to the index cache, (c) deadlock self-update when an old binary hard-errors on a bumped `config.json.format_version` (C-35) — the exact incompatibility self-update exists to fix. D6 does not require client-side routing: ownership is enforced by index CI (§2e/§2f), not at resolve time. The self-ref stays a **baked physical identifier constant** (exact ghcr.io repo fixed at migration step 2; manually bumped on the rare storage migration — self-management stays opinionated about its registry, as today). `__OCX_SELF_IMAGE` loopback test seam unchanged. **C-24 (rewritten)**: `self update`/`activate`/bootstrap perform **zero** sparse-index GETs; `ocx_cli_identifier()` returns the baked physical ref; a cold-cache offline shell start succeeds. |

- **C-25** A registry component that is **not** an exact known-registries key passes
  through unchanged (literal OCI host): `ghcr.io/foo`, `reg.corp/foo`, `localhost:5000/foo`
  all return `physical == requested`, zero index GETs.

### 2(c) Dual-name carrier (D9)

**Chosen shape (see §5 Trade-off T2):** a wrapper struct, not a field on `oci::Identifier`.

```rust
// crates/ocx_lib/src/registry_index/resolved.rs
/// An identifier after the early rewrite stage: the logical form the user typed
/// (`requested`) plus the physical form storage/lock/network operate on
/// (`physical`). When no rewrite occurred the two are equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentifier {
    requested: oci::Identifier,
    physical: oci::Identifier,
}

impl ResolvedIdentifier {
    pub fn identity(id: oci::Identifier) -> Self;              // requested == physical
    pub fn rewritten(requested: oci::Identifier, physical: oci::Identifier) -> Self;
    pub fn requested(&self) -> &oci::Identifier;              // display / report / lock alias
    pub fn physical(&self) -> &oci::Identifier;               // storage / network / lock repository
    pub fn was_rewritten(&self) -> bool;                     // requested != physical
}
```

- `oci::Identifier` is **untouched** — it stays the `Eq + Hash + Ord` storage key. The
  wrapper never reaches the store, index, or client (D11 preserved by construction).
- **C-26** `ResolvedIdentifier::identity(x)` → `requested() == physical() == x`,
  `was_rewritten() == false`.
- **C-27** For a rewritten pair, `physical()` is what every `PackageManager` task and
  store keying receives; `requested()` is what the report/display layer renders.

#### Lockfile record (D9: "logical name + physical ref + digest")

`LockedToolV2` (per discovery §5: `{ name, group, repository: Identifier, platforms }`)
gains one optional field:

```rust
pub struct LockedToolV2 {
    // … existing fields …
    /// The logical registry/repository the user declared, when it differed from
    /// the resolved physical `repository` (i.e. an index rewrite occurred).
    /// Absent when no rewrite happened (declared == physical). Additive, serde
    /// default → old locks read as `None`; stays lock-format V2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<String>,   // e.g. "ocx.rs/kitware/cmake"
}
```

- `repository` continues to store the **physical** identifier (bare `registry/repo`,
  unchanged storage-key semantics).
- **C-28** Writing a lock for a rewritten tool records `repository = "ghcr.io/ocx-contrib/cmake"`
  and `requested = "ocx.rs/kitware/cmake"` (registry/repo only — no tag/digest in `requested`).
- **C-29** Writing a lock for a non-rewritten tool omits `requested` entirely (skip-serialize);
  the on-disk V2 byte shape is unchanged for the corporate/direct-OCI case.
- **C-30** Reading a pre-indirection lock (no `requested` key) succeeds; `requested` = `None`.
- **C-31** The `declaration_hash` (RFC 8785 JCS over tools+groups declaration) is unaffected
  by `requested` — it hashes the *declaration* (ocx.toml), not the resolved lock body.
  Regression-lock: adding `requested` does not perturb `declaration_hash`.

### 2(d) Sparse-index HTTP client (D12/D13)

#### HTTP dependency choice

**Reuse `reqwest`** (workspace, 0.13, rustls-tls + webpki-root-certs). It is *already
compiled into the binary* via the `oci-client` fork (`external/rust-oci-client` →
`reqwest 0.13`, `default = ["rustls-tls", …]`), so adding it as a direct `ocx_lib`
dependency introduces **zero** new license/audit/binary-size surface — it is the boring,
already-paid-for choice. Match the fork's self-contained trust posture: rustls +
`webpki-root-certs` so the client works on distroless/CI hosts without a system trust
store (same rationale the OCI client documents). Rejected: `ureq` (blocking, would need
`spawn_blocking`), a bespoke `hyper` client (reinvents TLS/redirect/decompression), and
threading through the OCI transport (OCI-specific bearer dance and manifest media types
do not apply to a static CDN JSON fetch).

#### Transport seam (testability, mirrors `OciTransport`)

```rust
// crates/ocx_lib/src/registry_index/client.rs
/// Abstract HTTP transport for sparse-index fetches — the test seam. The native
/// impl is reqwest; tests inject a stub returning canned bodies/ETags/status,
/// exactly like `StubTransport` for the OCI client.
#[async_trait::async_trait]
pub trait SparseIndexTransport: Send + Sync {
    /// Conditional GET. `if_none_match` carries a cached ETag (or `None` for an
    /// unconditional fetch under `--remote`).
    async fn get(
        &self,
        url: &str,
        if_none_match: Option<&str>,
    ) -> Result<FetchOutcome, SparseIndexError>;
}

pub enum FetchOutcome {
    /// 200 — body + optional new ETag.
    Modified { body: Vec<u8>, etag: Option<String> },
    /// 304 — use the cached body.
    NotModified,
    /// 404 — no such index file.
    NotFound,
}
```

```rust
pub struct SparseIndexClient<T: SparseIndexTransport> {
    transport: T,
    cache: IndexCache,          // §cache
}

impl<T: SparseIndexTransport> SparseIndexClient<T> {
    /// Fetch + parse a package entry from `endpoint`, honoring `mode`.
    /// Returns `None` when the package is absent from the index (404).
    async fn fetch_entry(
        &self,
        endpoint: &str,
        namespace_repo: &str,           // e.g. "kitware/cmake" (repository, alias-registry stripped)
        mode: ChainMode,
    ) -> Result<Option<IndexEntry>, ResolveError>;

    /// Fetch + parse + version-gate the root `config.json`. Cached like entries.
    async fn fetch_config(&self, endpoint: &str, mode: ChainMode)
        -> Result<IndexConfig, ResolveError>;
}
```

#### Index path derivation

The index path is derived from the identifier's **repository** (alias-registry stripped),
so `ocx.rs/kitware/cmake` and the transitional `ocx.sh/kitware/cmake` both hit the same
file:

- **C-32** `namespace_repo` for identifier `{registry:"ocx.rs", repository:"kitware/cmake"}`
  is `"kitware/cmake"`; the fetched URL is `<endpoint>/p/kitware/cmake.json`.
- **C-33** `ocx.sh/kitware/cmake` (transitional alias) fetches the *identical* URL
  `https://index.ocx.rs/p/kitware/cmake.json` (both aliases share the index endpoint).

#### `config.json` format-version gate (D12)

- **C-34** `config.json` with `format_version` ≤ the client's supported version → parse
  succeeds; **unknown extra fields are ignored** (additive evolution).
- **C-35** `config.json` with `format_version` **greater** than supported → hard error
  `ResolveError::UnsupportedFormatVersion { found, supported }` (remediation: "upgrade ocx").
- **C-36** `config.json` is fetched at most once per resolver lifetime (cached in-memory
  after first fetch) and gates all entry fetches from that endpoint.

#### Freshness + ChainMode interplay (D13)

Conditional GET (`If-None-Match`/ETag → 304) mapped onto `ChainMode`:

| Mode | Index fetch behavior |
|---|---|
| `Default` | Conditional GET (send cached ETag). 200 → update cache; 304 → use cache; network error → fall back to cache **with WARN** if present, else `EndpointUnreachable`. |
| `Remote` | **Unconditional** GET (ignore cached ETag), always refresh cache. Network error → `EndpointUnreachable`. |
| `Offline` | **Never** touch network. Cache hit → resolve; cache miss → `OfflineNoCache { policy: "offline" }`. |
| `Frozen` | Same as `Offline` for the *resolution* step (resolution frozen to cache): cache miss → `OfflineNoCache { policy: "frozen" }`. |

- **C-37** `Default` + valid cached ETag + server 304 → resolves from cache, zero body
  transfer (stub returns `NotModified`).
- **C-38** `Offline` + cache miss → `ResolveError::OfflineNoCache` (→ exit 81), zero
  transport calls. Mirrors the `ChainMode::Offline` unpinned-tag → `PolicyBlocked` contract
  in `subsystem-oci.md` — one consistent offline story.
- **C-39** `Default` + transport error + present cache → resolves from cache, emits a
  single WARN naming the endpoint (self-heal on next clean, per project memory "no
  WARN/block on common benign states" — a reachable-but-slow CDN is benign).
- **C-40** `Remote` sends **no** `If-None-Match` header even when a cached ETag exists.
- **C-41** A lockfile-pinned install never constructs a `SparseIndexClient` request
  (resolver short-circuits on the exempt lock path — C-21). This is the offline-CI happy path.

#### On-disk cache location (state, not cache)

Per project memory (`state/` = persistent runtime state; `cache/` = regenerable bulk that
may be dropped anytime): the index cache is **offline-critical** (it is the offline source
of truth for resolution, like the local index), so it lives under **state**:

```
$OCX_HOME/state/registry-index/<endpoint-slug>/config.json.envelope
$OCX_HOME/state/registry-index/<endpoint-slug>/p/<namespace>/<package>.json.envelope
```

`<endpoint-slug>` = `to_slug(endpoint_url)` (reuse `StringExt::to_relaxed_slug`; keys the
cache by endpoint so two aliases sharing one endpoint share cache correctly, and two
endpoints never collide). Envelope shape (boring, one file):

```json
{ "etag": "\"abc123\"", "fetched_at": "2026-07-12T…Z", "entry": { …IndexEntry… } }
```

- **C-42** After a 200 fetch, an envelope with the response ETag is written; a subsequent
  `Default` fetch sends that ETag as `If-None-Match`.
- **C-43** Cache reads/writes use the existing `LockedJsonFile`/atomic-write primitives
  (no bespoke I/O); a corrupt envelope is treated as a cache miss (debug-logged, re-fetched),
  never a hard error.
- **C-44** Cache is keyed by endpoint-slug; `ocx.rs` and `ocx.sh` (same endpoint) resolve
  from one shared envelope for a given package.

### 2(e) Index entry schema v1 + config.json schema

Incorporates the three research additions (`research_sparse_index_formats.md` §4):
structured owners, `deprecated_message`, `latest_digest_hint`.

#### `config.json` (index root)

```json
{
  "format_version": 1
}
```

```rust
#[derive(Debug, Clone, Deserialize)]         // NO deny_unknown_fields — additive evolution (D12)
pub struct IndexConfig {
    pub format_version: u32,
    // future: endpoints, all.json pointer — ignored-if-unknown until this client learns them
}
```

- **C-45** `IndexConfig` deserializes `{"format_version":1,"future_field":true}` without
  error (additive), and rejects nothing on unknown keys.

#### Package entry `p/<namespace>/<package>.json` (schema v1)

```json
{
  "name": "ocx.rs/kitware/cmake",
  "repository": "oci://ghcr.io/ocx-contrib/cmake",
  "owners": [{ "github": "alice", "github_id": 123456 }],
  "status": "active",
  "deprecated_message": null,
  "yanked": [],
  "latest_digest_hint": null,
  "created": "2026-07-11"
}
```

```rust
#[derive(Debug, Clone, Deserialize)]         // NO deny_unknown_fields — additive
pub struct IndexEntry {
    pub name: String,                        // logical, e.g. "ocx.rs/kitware/cmake"
    pub repository: String,                  // "oci://ghcr.io/ocx-contrib/cmake"
    #[serde(default)]
    pub owners: Vec<IndexOwner>,
    #[serde(default)]
    pub status: EntryStatus,                 // active | deprecated | yanked
    #[serde(default)]
    pub deprecated_message: Option<String>,
    #[serde(default)]
    pub yanked: Vec<String>,                 // yanked version strings
    #[serde(default)]
    pub latest_digest_hint: Option<String>,  // advisory only — never a trust anchor
    #[serde(default)]
    pub created: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexOwner {                       // structured (research §4): survives GH username rename
    pub github: String,
    #[serde(default)]
    pub github_id: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    #[default] Active,
    Deprecated,
    Yanked,
}
```

- **C-46** The `repository` field parses `oci://<host>/<repo>` into the physical
  `oci::Identifier` registry (`<host>`) + repository (`<repo>`); a `repository` without
  the `oci://` scheme → `ResolveError::MalformedEntry`.
- **C-47** `latest_digest_hint` is **advisory**: the resolver never resolves *content* from
  it. It is carried onto `ResolvedIdentifier` for optional display / a defense-in-depth
  "does the digest I resolved live match what the index last saw" WARN — never a gate.
  (Test: resolution succeeds and fetches live `tags/list` even when `latest_digest_hint`
  is stale/mismatched; at most a WARN is emitted, never an error.)
- **C-48** `status: "yanked"` or the requested version ∈ `yanked[]` → resolution refused
  with `ResolveError::Yanked` (minimal leaf; full CLI yank semantics deferred per ADR).
- **C-49** `status: "deprecated"` resolves normally and surfaces `deprecated_message` as a
  single WARN (non-fatal) — a deprecated package still installs.
- **C-50** Unknown extra entry fields are ignored (additive; no `deny_unknown_fields`).

*Ownership note (D6):* the client does not verify ownership at resolve time — ownership is
a governance property enforced in the `ocx-sh/index` CI (§2f). The client trusts the
rendered entry.

### 2(f) `ocx-sh/index` repository

Separate repo (already created per ADR Ground Setup — `ocx-sh/index`, public, auto-merge
enabled, Cloudflare Pages deploy substrate live). This spec defines the render/validate CI
contract, announce transport, and reconcile cron. **This work is fully parallelizable with
all ocx-repo Rust work** (§8 Phase P0).

#### Layout

```
config.json                          # deployed as-is to Pages root (format_version)
p/<namespace>/<package>.json         # SOURCE OF TRUTH (git), one file per package
public/                              # RENDERED output (CI-generated) → Cloudflare Pages
  config.json
  p/<namespace>/<package>.json
.github/workflows/
  validate.yml                       # PR gate: schema + governance
  render-deploy.yml                  # main: render public/ → Pages
  announce.yml                       # repository_dispatch → regen one entry → PR
  reconcile.yml                      # nightly cron → regen all → PR/flag
schema/entry.schema.json             # JSON Schema for p/*.json (v1)
schema/config.schema.json
```

*Sharding note (research §5):* do **not** build prefix-sharding. The `p/<namespace>/…`
layout is already one level of natural sharding. The only watch trigger is
`p/ocx-contrib/` approaching GitHub's ~1,000–3,000-files-per-directory comfort zone in the
*source* tree; document that threshold in the repo README and move on.

#### `validate.yml` (PR gate) — contract

- **G-01** Every changed `p/*.json` validates against `schema/entry.schema.json` (v1).
- **G-02** `name` field equals the logical name derived from the file path
  (`p/<ns>/<pkg>.json` → `ocx.rs/<ns>/<pkg>`) — no path/name mismatch.
- **G-03** `repository` is a well-formed `oci://<host>/<repo>` and its host is on an
  allowlist (ghcr.io for the public catalog; extensible) — anti-squat / anti-exfil guard
  (BCR `repository` allowlist precedent).
- **G-04** **New entry file** (added path) → require the `new-package` label + human review;
  **never** auto-merge (D5 namespace gate). CI posts the label; branch protection blocks
  auto-merge without human approval.
- **G-05** **Existing entry refresh** whose regenerated content matches registry truth and
  is schema-green → eligible for auto-merge (D5). Yank/deprecate/transfer/`owners`/pointer
  changes → require human review regardless (D5 row 3), enforced by diffing the changed
  keys against a "human-review-required" key set (`repository`, `owners`, `status`, `yanked`).

#### `render-deploy.yml` (main) — contract

- **G-06** Renders `p/*.json` (source) → `public/p/*.json` and `config.json` →
  `public/config.json`, then deploys `public/` to the `ocx-index` Cloudflare Pages project
  (existing `deploy.yml` substrate). Rendering is currently identity (copy) — the split
  exists so a future non-trivial render (minify, `all.json` generation) has a home without
  changing the client contract.
- **G-07** Deploy is idempotent; re-running on an unchanged tree is a no-op.

#### `announce.yml` (doorbell, D4) — transport contract

**v1 transport = `repository_dispatch` + fine-grained PAT** (per
`research_index_announce_bots.md` Recommendation #2 — skip the GitHub App at 42–400
packages; it conflicts with the "zero production services" driver).

- **G-08** Trigger: `repository_dispatch` with `event_type: "announce"` and
  `client_payload: { package: "<namespace>/<pkg>" }`. Authenticated by a fine-grained PAT
  (contents + actions:dispatch on `ocx-sh/index`) held as an org secret in the publisher
  org (`ocx-contrib`). The payload carries **only** a package identifier (Go-proxy
  doorbell: the payload cannot lie — every written field is re-derived, never trusted from
  the payload).
- **G-09** The bot job regenerates `p/<namespace>/<pkg>.json` **entirely from registry
  truth** (`tags/list` + manifests against `ghcr.io/ocx-contrib/<pkg>`), never from the
  dispatch body. `latest_digest_hint` is populated from the same registry read.
- **G-10** Regen retries the manifest fetch with bounded backoff (a handful of attempts
  over ~1 min) before giving up — covers the announce/registry propagation race
  (`research_index_announce_bots.md` Pitfalls).
- **G-11** Idempotent + cascade-safe: N announces of the same package converge to one
  result; regen reads post-cascade tag state, so the publisher never enumerates what
  changed. Diff → open a PR routed by the `validate.yml` merge policy (new file → human;
  green refresh → auto-merge).

#### `reconcile.yml` (nightly cron, D4) — contract

- **G-12** Nightly: regenerate **every** entry from registry truth; diff against the
  committed source; open one PR with all drift (self-heal for pushes that bypassed announce).
- **G-13** **Anomaly rule (new, from `research_index_announce_bots.md` Pitfall #2):** if
  reconcile detects that an **already-published** version's digest *changed* (immutable
  version mutated), that is a **hard-stop integrity anomaly** — flag for a human (label +
  fail the job), **never** silently overwrite. Routine additions/removals are normal drift
  and auto-PR as usual. This is an ADR amendment candidate (§9).

---

## 3. User Experience Scenarios

Format: **action → outcome → error cases**. Each maps to acceptance-test contracts.

### S1 — Install via logical name (cache hit / index hit)
`ocx package install ocx.rs/kitware/cmake:3`
→ resolver: `ocx.rs` is an index key → GET `p/kitware/cmake.json` (304 or state-cache) →
physical `ghcr.io/ocx-contrib/cmake:3` → normal OCI install → symlink.
Output shows `ocx.rs/kitware/cmake:3` (requested); `resolved_from` field / verbose shows
`ghcr.io/ocx-contrib/cmake`.
- **Error:** physical fetch 404 (index points at a nonexistent tag) → `TagNotFound` (79),
  message references **physical** ref (D9 — needed for debugging).

### S2 — Logical name missing from index
`ocx package install ocx.rs/kitware/nonesuch`
→ resolver GET `p/kitware/nonesuch.json` → 404 → `ResolveError::EntryNotFound` → exit **79**.
Remediation: "no package `ocx.rs/kitware/nonesuch` in the index; check the name or browse
https://ocx.rs".

### S3 — Index endpoint unreachable (online)
`ocx package install ocx.rs/cmake` with `index.ocx.rs` down, no cache.
→ transport error, no cache fallback → `ResolveError::EndpointUnreachable` → exit **69**.
Remediation: "index endpoint https://index.ocx.rs unreachable; retry, or set
`[mirrors]`/pin a lock for offline resolution." (GHCR/CDN weather relief valve — research §7.)
- **With stale cache present:** resolves from cache + single WARN (C-39). Exit 0.

### S4 — Index endpoint unreachable (offline / frozen)
`ocx --offline package install ocx.rs/cmake`, cache miss.
→ `ResolveError::OfflineNoCache { policy: "offline" }` → exit **81** (PolicyBlocked).
Remediation: "offline: `ocx.rs/cmake` is not cached; run once online or pin a lock."
Consistent with the existing `--offline` unpinned-tag → 81 story.
- **Cache hit:** resolves from state cache, zero network, exit 0 (the offline-CI path).

### S5 — Yanked package/version
`ocx package install ocx.rs/acme/tool:1.2.3` where `1.2.3 ∈ yanked[]`.
→ `ResolveError::Yanked { name, version, reason }` → exit **65**.
Remediation: "version 1.2.3 of ocx.rs/acme/tool is yanked (<reason>); pin a different version."
(Minimal leaf; full resolve-vs-lock yank behavior deferred per ADR.)

### S6 — Unknown format_version
`ocx package install ocx.rs/cmake` where `config.json.format_version = 2` and this ocx
supports 1.
→ `ResolveError::UnsupportedFormatVersion { found: 2, supported: 1 }` → exit **65**.
Remediation: "the ocx.rs index uses format version 2; upgrade ocx (`ocx self update`)."

### S7 — Direct OCI host unchanged
`ocx package install ghcr.io/myorg/tool:1` (or a private `reg.corp/team/tool`).
→ `ghcr.io` is not a known-registries key → literal OCI host → `physical == requested` →
**zero** index GETs → existing pipeline. Corporate/private story is untouched (D-context #4).
Exit 0 (or normal OCI errors — 80 auth, 69 unreachable — unchanged).

### S8 — Default-registry bare name after switch to ocx.rs
`ocx package install cmake:3` with shipped defaults (`[registry] default = "ocx.rs"`).
→ `with_domain("ocx.rs")` → `ocx.rs/cmake:3` → resolver → `ghcr.io/ocx-contrib/cmake:3`.
Bare names resolve through the public index by default.
- **Corporate override:** `[registry] default = "corp"`, `[registries.corp] url =
  "oci+https://reg.corp"` → `cmake:3` → `corp/cmake:3` → `reg.corp/cmake:3`, no index.

### S9 — Transitional `ocx.sh` alias
`ocx package install ocx.sh/kitware/cmake:3` during migration.
→ `ocx.sh` is an index key (baked default, same endpoint) → GET
`https://index.ocx.rs/p/kitware/cmake.json` → physical ghcr.io ref. Old-name installs keep
working; `ocx.sh` alias covers stragglers (D14).

---

## 4. Error Taxonomy

New error type in `registry_index/error.rs`, following `quality-rust-errors.md`
(thiserror, `#[non_exhaustive]`, lowercase no-period messages, `#[source]` on wrapping
variants) and mapped to exit codes per `quality-rust-exit_codes.md`. **No new `ExitCode`
variant is introduced** — every failure maps onto an existing sysexits-aligned code
(YAGNI; the exit-code rule cautions against adding codes).

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolveError {
    #[error("no package '{name}' in the index")]
    EntryNotFound { name: String },

    #[error("index endpoint '{endpoint}' unreachable")]
    EndpointUnreachable { endpoint: String, #[source] source: SparseIndexError },

    #[error("{policy}: package '{name}' is not cached and resolution may not use the network")]
    OfflineNoCache { name: String, policy: &'static str },   // "offline" | "frozen"

    #[error("index format version {found} is newer than this ocx supports (max {supported})")]
    UnsupportedFormatVersion { found: u32, supported: u32 },

    #[error("malformed index entry for '{name}'")]
    MalformedEntry { name: String, #[source] source: EntryParseError },

    #[error("malformed index config at '{endpoint}'")]
    MalformedConfig { endpoint: String, #[source] source: EntryParseError },

    #[error("version '{version}' of '{name}' is yanked")]
    Yanked { name: String, version: String, reason: Option<String> },
}
```

| Variant | Exit code | Mnemonic | Remediation text (CLI boundary, sentence-case OK) |
|---|---|---|---|
| `EntryNotFound` | **79** `NotFound` | package 404 at index layer | "No package `<name>` in the index; check the name or browse https://ocx.rs." |
| `EndpointUnreachable` | **69** `Unavailable` | registry/index unreachable | "Index endpoint `<endpoint>` is unreachable; retry, or pin a lock for offline use." |
| `OfflineNoCache` | **81** `PolicyBlocked` | deliberate policy refusal | "`<policy>`: `<name>` is not cached; run once online or pin a lock." |
| `UnsupportedFormatVersion` | **65** `DataError` | data this binary can't parse | "The index uses format version `<found>`; upgrade ocx (`ocx self update`)." |
| `MalformedEntry` / `MalformedConfig` | **65** `DataError` | corrupt index data | "The index entry for `<name>` is malformed; report to the catalog maintainers." |
| `Yanked` | **65** `DataError` | version withdrawn | "Version `<version>` of `<name>` is yanked (`<reason>`); pin a different version." |

**classify_error wiring** (`crates/ocx_cli`, per exit-code rule's free-function pattern):
add one downcast arm for `ResolveError`, matching each variant to the code above. Because
the resolver runs *before* `PackageManager`, `ResolveError` surfaces as a standalone lib
error at the command boundary (composed into `anyhow`), **not** wrapped in `PackageError`.

- **C-51** Each `ResolveError` variant classifies to the exit code in the table; a test
  locks the full mapping (exhaustive match so a new variant compile-errors until classified).
- **C-52** `EndpointUnreachable` carries its `SparseIndexError` source so `{err:#}` prints
  the full chain (transport cause visible for debugging).

---

## 5. Trade-off Analysis (open sub-decisions only)

Only genuinely open sub-decisions get a table; settled ADR decisions are not re-opened.
Weighted criteria: **Correctness** (0.30), **Simplicity/KISS** (0.25), **Reversibility**
(0.20), **Offline-CI fidelity** (0.15), **Reporting fidelity** (0.10).

### T1 — How `[registry] default` interacts with the rewrite

| Option | Correctness | Simplicity | Reversibility | Offline | Reporting | Weighted |
|---|---|---|---|---|---|---|
| **A. `resolved_default_registry` returns the KEY; rewrite handles oci+ and index+ uniformly** | High | High (one code path, symmetric) | High | High | n/a | **0.90** |
| B. Split: extract host for oci+, key for index+; rewrite only index+ | Med (asymmetric) | Low (two behaviors on one field) | Med | High | n/a | 0.62 |
| C. Keep `resolved_default_registry` untyped (bare host); rewrite is a wholly separate concern with its own `url` format | Low (two `url` formats) | Low | Low (forks the field) | High | n/a | 0.48 |

**Recommendation: A.** The rewrite is D8's mandatory early stage anyway; making it the
single dereference point (for both index and direct-OCI aliases) is the symmetric, KISS
choice. **Risk it introduces:** every bare-name path now depends on the rewrite running —
mitigated because `resolve_identifiers` is the one seam right after `with_domain`, exercised
by C-18/C-25. **Reversibility:** high — `resolved_default_registry` is one function, one
call site (`context.rs:209`), one consumer.

### T2 — Dual-name carrier type shape

| Option | Correctness | Simplicity | Reversibility | Offline | Reporting | Weighted |
|---|---|---|---|---|---|---|
| **A. `ResolvedIdentifier { requested, physical }` wrapper** | High (storage key untouched) | High | High | High | High | **0.93** |
| B. `alias: Option<String>` field on `oci::Identifier`, excluded from Eq/Hash/Ord (manual impl) | Low (fragile — one derive slip poisons cache/dedup keying) | Low | Low (touches the universal type) | High | High | 0.50 |
| C. Report-level side map `HashMap<physical, requested>`; physical-only everywhere else | Med | Med | High | High | Med (lock loses logical — violates D9) | 0.66 |

**Recommendation: A.** `oci::Identifier` is the `Eq+Hash+Ord` storage key used everywhere;
adding an equality-excluded field (B) is a latent cache-poisoning footgun (two "equal"
identifiers that hash differently). The wrapper (A) keeps the storage key pristine (D11
holds by construction) and threads `requested` only through the boundary layers
(command/report/lock). **Reversibility:** high — the wrapper is additive; deleting it
reverts to physical-only.

### T3 — Record the requested/logical alias in `ocx.lock`?

| Option | Correctness | Simplicity | Reversibility | Offline | Reporting | Weighted |
|---|---|---|---|---|---|---|
| **A. Record `requested: Option<String>` per tool (skip-serialize when absent)** | High | High (additive, V2-compatible) | High | High | High | **0.91** |
| B. Physical-only in lock (drop D9's logical-in-lock) | Med (loses provenance; a re-render can't show what the user pinned) | High | High | High | Low | 0.72 |
| C. Record only the alias registry as lock metadata (not per-tool) | Med | Med | Med | High | Med | 0.63 |

**Recommendation: A.** One optional field, `#[serde(default, skip_serializing_if)]`, keeps
the byte shape identical for the corporate/direct-OCI case (C-29) and stays lock-format V2
(no version bump). It satisfies D9 (logical + physical + digest all in the lock), enables
honest `ocx run`/report display from a locked project without a live index fetch, and is
fully reversible (drop the field → old behavior). Cost is negligible.

---

## 6. Edge Cases

| # | Edge case | Handling |
|---|---|---|
| E1 | **Name that looks like a host** (`myregistry.com/foo`) | Registry component is not an exact known-registries key → literal OCI host, zero index GET (C-25). Exact-match-only rule (D10) — no fuzzy/probing. |
| E2 | **Index entry points at a private registry** (`oci://ghcr.io/private/x`) | Resolver returns the physical ref; the subsequent OCI fetch does the normal auth dance. 401 → `AuthError` (80) via the existing client path — not a resolver concern. |
| E3 | **Mirror-of-physical chain** (`ocx.rs → ghcr.io → [mirrors."ghcr.io"]`) | Index resolves to `ghcr.io/ocx-contrib/cmake` (physical); `Client::transport_reference` then rewrites `ghcr.io` → corp mirror. Order `logical → index → physical → mirror` (D11) holds automatically — stores keyed by `ghcr.io`, transport by mirror. This is the GHCR relief valve (research §7). |
| E4 | **Alias typo falls through to DNS** (`ocx.sr/cmake`) | Not a key → literal host → DNS resolution failure → `EndpointUnreachable`/network error at the OCI layer. Documented residual risk (D10); mitigated for the real names by shipping both `ocx.rs` + `ocx.sh` in the default map. |
| E5 | **Lockfile from the pre-indirection era** | Old lock's `repository` may hold a *logical* name (e.g. `ocx.sh/cmake`) because no rewrite existed when it was written. Lock-read is exempt (D11), so it would hit `ocx.sh` literally → fails after Artifactory retirement. **Handling:** a locked `repository` whose registry component matches an *index-typed* key is treated as needing re-resolution — error with `"lock predates the index migration; run 'ocx lock' to re-resolve"` (a targeted hint) rather than silently fetching a retired host. Migration step 5 (`ocx lock` re-resolve) is the intended remedy. |
| E6 | **Managed config distributing `[registries]`** | Managed tier folds into the registry map like `[mirrors]` (forward-compat, no `deny_unknown_fields` on `Config`). **But** the managed *source's own* resolution uses the pre-managed map (C-23) — the managed tier cannot alias the registry it is itself fetched from (bootstrap ordering). Fleet operators get registry aliases fleet-wide via the same managed-config machinery. |
| E7 | **Two aliases of one physical registry** (`ocx.rs` + `ocx.sh` → same index → same ghcr.io repo) | Both resolve to identical physical refs → identical storage keys → correct layer sharing (D11: "two aliases of one physical registry share correctly"). Index cache shared via endpoint-slug (C-44). |
| E8 | **Index entry `repository` host not on the CI allowlist** | Caught in `ocx-sh/index` `validate.yml` (G-03), never reaches a client. If a malformed entry slips through, the client's `MalformedEntry` (65) fires on the `oci://` parse (C-46). |
| E9 | **`unknown/unknown` platform in the ghcr.io image index** (buildx attestation pollution, research §6) | Not a resolver concern — the resolver only produces the physical `registry/repo`; platform selection is the existing `Index::select`/`Platform` path. Flagged for the `ocx-contrib` publish pipeline (disable buildx provenance/sbom) in the GHCR research, not this client. |
| E10 | **Concurrent resolves of the same name** | Fan-out is order-preserving (C-20); duplicate names in one batch each hit the cache (second is a cache/304 hit). No singleflight needed at 42–400 packages (YAGNI; note the upgrade path if batch installs of hundreds of distinct new names become common). |
| E11 | **`latest_digest_hint` stale vs live tags/list** | Advisory only (C-47): resolution uses live `tags/list`; a mismatch is at most a WARN, never a gate. Defense-in-depth against the TOFU trade-off (D3) without pulling the signing ADR forward. |

---

## 7. Migration / Rollout

Pre-1.0 stance: **no migration code** (project memory `project_breaking_compat_next_version.md`).
Breaking changes just break; user docs carry no "former form removed" prose (project memory
`feedback_no_migration_prose_in_docs.md`). The sequence below is *operational* ordering, not
in-code compatibility shims.

**Ordering (extends ADR Migration Plan §297–316):**

1. **P0 — `ocx-sh/index` repo stands up first** (parallel with Rust work). Seed
   `config.json` (`format_version: 1`) + 42 entries pointing at `ghcr.io/ocx-contrib/<pkg>`;
   `validate.yml` + `render-deploy.yml` green; entries served at `index.ocx.rs`. Gate:
   `curl https://index.ocx.rs/p/<ns>/<pkg>.json` returns each entry.
2. **Republish 42 packages** to `ghcr.io/ocx-contrib/*` with **`ocx.rs/...` embedded
   identifiers** (required for the D6 ownership check; clean republish, no dual-identifier
   compat). Sequencing: batch by namespace; bounded push concurrency + retry/backoff in the
   `ocx-contrib` publish CI (GHCR 429 mitigation, research §5) — **`ocx-mirror`-repo scope,
   not ocx core**. Gate: each package's manifest embeds its `ocx.rs/...` identifier.
3. **CLI work lands** (this spec's Rust phases P1–P6). Ship the baked default map with
   `[registry] default = "ocx.rs"` and both `ocx.rs`/`ocx.sh` index aliases. The
   `DEFAULT_REGISTRY` constant flips `ocx.sh` → `ocx.rs` (D14). Gate: `task verify` +
   acceptance tests (§8 P6) green.
4. **Coworker cutover.** One-line registry switch is *already the default* after step 3
   (shipped default = `ocx.rs`); coworkers on the old `ocx.sh` bare name keep working via
   the `ocx.sh` alias. Re-run `ocx lock` in each project to re-resolve stale
   pre-indirection locks (E5). Gate: a coworker `ocx install cmake` resolves via the index.
5. **Retire Artifactory + own server** — **gated on:** (a) all 42 packages served from
   ghcr.io via the index; (b) no lockfile in active projects still pins a bare `ocx.sh`
   Artifactory ref (re-lock done); (c) `ocx.sh` alias in the default map points at the
   index endpoint (not Artifactory), so any residual `ocx.sh/...` name index-resolves to
   ghcr.io rather than the dying server. Only then decommission.
6. **`ocx.sh` sunset** (post-migration, optional): keep the alias in the default map
   indefinitely (permanent-ish per D14); domain renewal is optional. No code change to sunset.

**Backward-compat stance summary:** lock schema addition is additive/serde-default
(readable old→new and new→old within V2); config `url` typing is a hard break on an
unreleased field (no shim); no runtime migration branches anywhere.

---

## 8. Task Decomposition Draft (contract-first TDD)

Each phase runs the **Stub → Specify → Implement → Review** cycle (workflow-feature). Rust
work (ocx repo) is separated from `ocx-sh/index` repo work (P0). Contract IDs from §2–§4
are the test targets; a phase is done when its contracts have failing-then-passing tests
and `task rust:verify` is green.

| Phase | Deliverable | Contracts | Depends on | Parallelizable |
|---|---|---|---|---|
| **P0** | `ocx-sh/index` repo: layout, `validate.yml`, `render-deploy.yml`, `announce.yml`, `reconcile.yml`, JSON schemas | G-01…G-14 | — | **Yes** (separate repo; run alongside all Rust) |
| **P1** | Config typing: `RegistryEndpoint`, `RegistryConfig::parse_url`, `resolve_registry_map`, baked `default_registry_map()`, `resolved_default_registry` redefinition, `OCX_REGISTRIES` env + `OcxConfigView` forwarding | C-01…C-17 | — | **Yes** (∥ P2-stub, P3) |
| **P2** | Sparse-index client: `SparseIndexTransport` trait + reqwest impl + stub, `IndexEntry`/`IndexConfig`/`IndexOwner`/`EntryStatus`, on-disk state cache, conditional-GET + ChainMode, format-version gate | C-32…C-50 | P1 (`RegistryEndpoint`) | Partial (schema types + stub ∥ P3) |
| **P3** | Dual-name carrier `ResolvedIdentifier` + `LockedToolV2.requested` field + serde/schema | C-26…C-31 | — | **Yes** (∥ P1, P2) |
| **P4** | `RegistryResolver` (map + client + ChainMode), `Context::resolve_identifiers` CLI seam, bypass-site routing (compose exempt, managed pre-map, setup self-ref exempt, lock-read exempt) | C-18…C-25, C-25b, C-41 | P1, P2, P3 | No (integration) |
| **P5** | Error taxonomy `ResolveError` + `classify_error` arm, `resolved_from` report field on `InstallEntry` + siblings (Printable), display/verbose/error rules (D9) | C-51, C-52, S1–S9 | P3, P4 | No |
| **P6** | Docs (website: `[registries]` config, resolution model, offline behavior, GHCR mirror relief valve), acceptance tests (`test/tests/test_registry_indirection.py`), `subsystem-registry-index.md` rule, `task schema:generate` regen, product-context refresh | S1–S9 end-to-end | P1–P5, P0 | No (final) |

**Dependency graph:**

```
P0 ───────────────────────────────────────────────► (independent, separate repo)
P1 ──┐
P3 ──┼──► P4 ──► P5 ──► P6
P1 ─►P2 ─┘
```

**Critical path:** P1 → P2 → P4 → P5 → P6. P3 and P0 run off the critical path. P1 and P3
are the two foundation phases and should start in parallel; P2 begins as soon as
`RegistryEndpoint` (P1) exists.

**Documentation surfaces to update (P6, per project memory "plans must list docs"):**
`website/src/docs/reference/environment.md` (`OCX_REGISTRIES`), a new
`website/src/docs/reference/registries.md` (or a section in the config reference), the
config schema (`task schema:generate` → `website/src/public/schemas/config/v1.json`
auto-includes the typed `url` doc), the lock schema doc (new `requested` field),
`website/src/docs/user-guide.md` (three-store + resolution model), the GHCR mirror relief-valve
troubleshooting note (research §7), `product-context.md` (candidate Nesbitt citation +
public-catalog positioning), and the new `subsystem-registry-index.md` + `.claude/rules.md`
catalog row.

---

## 9. ADR Amendments (proposed — do NOT edit the ADR here)

Research since the ADR motivates these additive amendments to
`adr_public_index_registry_indirection.md`. Each is additive (no D1–D15 reversal).

1. **Entry schema (D3 Technical Details):** adopt the three
   `research_sparse_index_formats.md` §4 additions — (a) `owners` as structured
   `{ github, github_id }` objects (survives GitHub username renames; cheap now at 42
   packages, expensive to backfill — same timing logic as D14); (b) optional
   `deprecated_message: string|null` (free-text deprecation UX, distinct from `status`);
   (c) optional `latest_digest_hint: string|null` — **advisory, non-authoritative**,
   populated by the regen bot, a defense-in-depth hedge against the D3 TOFU trade-off that
   does not pull the deferred signing ADR forward.

2. **Announce credential (D4 / Ground Setup):** pin **v1 = `repository_dispatch` +
   fine-grained PAT** per registered namespace (issued during the namespace's human-reviewed
   registration PR, held as an org secret in the publisher org). Explicitly **defer OIDC
   Trusted Publishing to v2** (Deferred section) — mirrors crates.io/npm's direction but
   costs infra OCX does not need at 42–400 packages (`research_index_announce_bots.md` Rec
   #2/#3).

3. **Reconcile anomaly rule (D4 reconcile-cron description):** add — if the nightly
   reconcile detects that an **already-published version's digest changed** (immutable
   version mutated), treat it as a **hard-stop integrity anomaly** (flag for a human, fail
   the job), never a silent overwrite (`research_index_announce_bots.md` Pitfall #2; no
   studied system auto-heals an integrity violation).

4. **GHCR mirror relief valve (D1/D11, docs):** state explicitly that the already-shipped
   `[mirrors."ghcr.io"]` config is GHCR's designated relief valve for the confirmed,
   recurring regional CDN (Fastly) degradation and any future public-pull rate limit — a
   one-line troubleshooting-doc addition, not new engineering (`research_ghcr_constraints.md`
   Recommendation).

5. **Announce/registry propagation race (D4):** the regen bot must retry the manifest fetch
   with bounded backoff (a handful of attempts over ~1 min) before giving up; the nightly
   reconcile is the correctness backstop, not the happy-path fetch
   (`research_index_announce_bots.md` Pitfalls).

---

## 10. Post-Review Amendments (review round 1, 2026-07-12)

Applied after the three-perspective review panel (spec-compliance / architect trade-off /
SOTA gap). Each is authoritative over any conflicting earlier text.

1. **Setup self-ref: Exempt, not Routed** — §2b table row + C-24 rewritten in place (see
   there for full rationale). The plan's P4 implements the exempt path.
2. **Local-only verbs never touch the sparse index** — `which` / `deps` / `uninstall` /
   `select` / `deselect` operate on already-installed packages; physical identity is
   derivable from local state (tag store / symlink store / lock). Routing them through
   `resolve_identifiers` would give a cold-cache offline `uninstall <logical>` a spurious
   `EndpointUnreachable`. **C-25b (new)**: a local-only verb on an installed package
   performs zero sparse-index GETs and succeeds offline with a cold index cache. Sparse
   resolution is for fetch verbs (`install`, `pull`, `add`, `update`, `index update`, …).
3. **Seam ordering made structural where cheap** — P4 should implement `transform_all` +
   `resolve_identifiers` behind one shared helper for fetch verbs so the C-22 ordering
   guard (`resolve before compose`) is structural, not a per-command convention. Full
   fusion into `options::Identifier` is NOT required (deferred; revisit if the helper
   proves insufficient).
4. **Bounded retry before cache-fallback (C-39 extension)** — mirror cargo's proven
   `net.retry = 3` shape: `SparseIndexClient::get` retries transient network errors
   (connect/timeout/5xx) up to 3 attempts with short backoff **before** classifying
   `EndpointUnreachable` / falling back to cached state. Cargo precedent:
   `net.retry`/`http.timeout`/`http.low-speed-limit` (Cargo Book, Configuration).
5. **Announce payload validation (G-08 extension)** — the `client_payload.package`
   identifier is untrusted external input used in a path join and an OCI ref. `announce.yml`
   MUST validate it against `^[a-z0-9][a-z0-9._-]*(/[a-z0-9][a-z0-9._-]*)+$` (and reject
   `..`, absolute paths) **before any use**; never interpolate it into `run:` shell text
   (env-var indirection only). Block-tier boundary rule (`quality-core.md`).
6. **Sibling-repo CI hardening explicit** — `ocx-sh/index` workflows (validate /
   render-deploy / announce / reconcile) carry `permissions:` default-deny minimal grants
   and SHA-pinned actions. `subsystem-ci.md` conventions apply but its path scope does not
   fire in the sibling repo — restated here so P0 cannot silently skip it (G-14, new).
7. **Cloudflare Cache Rule guardrail (ops invariant)** — Pages serves
   `Cache-Control: public, max-age=0, must-revalidate` + ETag, which the freshness design
   (C-37/C-42) depends on; Cloudflare's default cache level does not cache `.json`. Never
   add a zone Cache Rule enabling caching for `*.json` on `index.ocx.rs` — it would serve
   stale entries without origin revalidation. Record in the index-repo README + P6
   troubleshooting docs.
8. **The published wire contract is THE One-Way Door** — the moment `index.ocx.rs` serves
   a client: entry field shapes (`repository: "oci://host/repo"`, name/path derivation),
   the `/p/<ns>/<pkg>.json` layout, and root `config.json` become an ecosystem contract
   that is only additively extensible; any breaking change requires a `format_version`
   break that strands old clients. Config `url` prefix, lock `requested`, and
   `ResolvedIdentifier` are all reversible pre-1.0 surfaces by comparison.
9. **§1 tree diagram superseded by §2a** — parsed-endpoint types (`RegistryEndpoint`,
   `default_registry_map()`, resolver fn) live in `config/registry.rs`; `registry_index/`
   has **no** `endpoint.rs`. The `RegistryMap` name in the §1 tree is a stale draft name —
   the resolved shape is §2a's `ResolvedRegistries`.
10. **P1 test churn** — existing unit test `resolved_default_registry_returns_url_from_named_entry`
    (`config.rs:468`) inverts under C-14 (function now returns the KEY): rewrite it, don't
    just add new tests.
11. **Trade-off numeric weights are qualitative shorthand** — the T1–T3 decisions stand on
    the prose rationale; the numeric scores are not derived from a grounded scale and
    should not be cited as evidence.

---

## 11. Codex Gate Amendments (cross-model review, gpt-5.6-sol, 2026-07-12)

One-shot adversarial pass returned 14 Block + 2 Warn findings; all triaged actionable at
contract level (no ADR reversal). Authoritative over conflicting §2–§8 text, same rule as §10.

### Client contracts

1. **C-25b/C-25c — local alias record (fixes: offline local-only verbs impossible without
   reverse mapping).** At install/pull time the resolver KNOWS the `requested → physical`
   pair; persist it in a local alias map under `$OCX_HOME/state/registry-index/aliases/`
   (per-store family, same locking discipline as tag store). **C-25c (new):** a successful
   install of a rewritten identifier writes the alias record. **C-25b (amended):**
   local-only verbs resolve logical→physical via (1) alias map, (2) sparse-index cache —
   never network; cold state on both → targeted error naming the alias miss (not
   EndpointUnreachable).
2. **C-46 (extended) — response identity binding + pointer qualification.** After fetch AND
   on every cache read the client verifies the entry names the requested package:
   compare the **namespace/package path** plus the **canonicalized registry** (requested
   registry mapped through the known-registries table — an `ocx.sh/<ns>/<pkg>` alias
   request matches the entry whose `name` is `ocx.rs/<ns>/<pkg>` because both keys point
   at the same index endpoint; consistent with C-33/S9/E7). Mismatch = `MalformedEntry`,
   cache entry purged. Literal string equality on the requested prefix is WRONG. `repository` MUST be an unqualified
   `oci://host/repo` — embedded tag/digest/port-less-path violations = `MalformedEntry`.
   The resolver copies the requested tag/digest onto the physical identifier verbatim;
   entry data can never override them.
3. **C-20b (new) — partial-batch semantics.** `resolve_all` returns per-identifier
   outcomes (`Vec<Result<ResolvedIdentifier, ResolveError>>`, input order); command layer
   applies OCX's existing per-package result model (one bad name doesn't abort valid ones;
   deterministic diagnosis order). Fail-fast only where the command already fails fast.
4. **C-36 (amended) — format gate per endpoint.** `config.json` gate state is keyed by
   endpoint URL (not resolver-global); failure state is NOT cached (retried next resolve).
5. **C-44 (amended) — injective cache key.** Cache directory key =
   `<relaxed-slug>-<first 8 hex of sha256(endpoint URL)>`. Slug alone is display-only.
6. **C-54 (new) — duplicate-resolve serialization.** Concurrent resolves of the same
   (endpoint, name) share one fetch via the existing `utility/singleflight.rs` primitive;
   a cache write must not regress a newer generation (compare-and-swap on envelope
   `fetched_at`/ETag generation).
7. **C-53 (new) — HTTP boundary hardening.** Transport: redirects disabled by default
   (at most 2 same-origin HTTPS redirects, no scheme downgrade ever); response size caps
   (config.json 64 KiB, entry 256 KiB, decompressed); total request timeout (30 s) +
   low-speed abort mirroring cargo's `http.low-speed-limit`; applies before JSON parse.

### Index-repo governance

8. **G-15 (new) — D6 ownership proof executed.** `validate.yml` (new/changed entries) and
   `announce.yml` (refresh) fetch the physical repo's manifest and verify the embedded
   canonical identifier equals the entry's logical `name`. Allowlist + syntax checks alone
   are insufficient.
9. **G-16 (new) — privileged/unprivileged workflow split.** Public-PR validation runs
   unprivileged (no secrets, `pull_request` event, read-only). Labeling/auto-merge runs in
   a separate privileged job that executes ONLY base-branch code; `pull_request_target`
   never checks out or executes PR-head content.
10. **G-17 (new, amends G-08's credential wording) — announce abuse bounds.** Announce
    secret is namespace-scoped — one fine-grained PAT per registered namespace, stored in
    that publisher's own repo/org, never a single shared org secret (supersedes G-08's
    "org secret" phrasing); `announce.yml` uses per-package concurrency
    groups (`concurrency: announce-<ns>-<pkg>`, cancel-in-progress) + schema-validated
    payload; ownership binding of caller→package stays v1-weak (PAT possession) — full
    binding is the OIDC v2 deferred item.
11. **G-09 (amended) — field provenance partition.** Registry-derived (regenerated every
    announce/reconcile): `latest_digest_hint`, refresh timestamp; plus a **read-only**
    existence/ownership probe of `repository` (feeds G-15 — verification only, NEVER a
    write path: the committed `repository` value only changes via a human-reviewed PR,
    G-05). Human-governed (preserved verbatim from the committed entry, never
    regenerated): `repository`, `owners`, `status`, `deprecated_message`, `yanked`,
    `created`. "Regenerate from registry truth" applies to the first set only.
12. **G-13 (amended) — reconcile needs state.** The reconcile job maintains a
    bot-committed observation file (`state/observed-digests.json`: per entry, per published
    cascade tag → last digest). Digest change on an already-observed tag = the hard-stop
    anomaly; absence of prior observation = first sight, recorded not flagged.
13. **G-18 (new) — reconcile enablement gate.** Reconcile stays disabled (or dry-run,
    no-mutation) until the 42-package republish batch is complete and parity-verified (M-1);
    prevents "missing repo = normal drift" auto-actions during partial migration.

### Migration gates (amend §7)

- **M-1 (gates step 2→3):** recorded per-package parity matrix (tags × platforms ×
  child manifests vs Artifactory) + an actual `ocx install <logical-name>` smoke pull per
  package — `curl 200` is not the gate.
- **M-2 (gates step 5):** old-client adoption — Artifactory retires only after coworker
  binaries are on an indirection-capable version (old binaries hit Artifactory directly;
  lockfile audit alone is insufficient). Minimum: version check across active consumers +
  announced compatibility window.
- **M-3:** reconcile enablement per G-18 ordering.

---

## 12. Naming Reversal — canonical stays `ocx.sh`, `ocx.rs` parked (ADR Amendment A1, 2026-07-12)

Owner decision. Authoritative over ALL earlier text, same rule as §10/§11. **Every
`ocx.rs/...` example in §1–§11 reads as `ocx.sh/...`.** Mechanics unchanged — only the
canonical name and endpoint host change (two-plane separation, D10/D15).

1. **Baked map (C-08 area):** single key — `ocx.sh → index+https://index.ocx.sh`. No
   `ocx.rs` alias row. Re-activation later = additive row (two-way door).
2. **No default flip:** `DEFAULT_REGISTRY` stays `ocx.sh`; P6 loses the flip task. The
   `ocx_cli_identifier()` baked physical constant is unaffected (self-ref exempt, §10.1).
3. **Entry names:** `ocx.sh/<ns>/<pkg>`. G-02 path/name derivation unchanged in shape.
4. **Scenarios:** S8 collapses to "bare name defaults to `ocx.sh` and index-resolves —
   observable behavior change is only the resolution path, not the name." S9
   (transitional alias) **dropped from v1 scope**; its contracts (C-33 alias handling,
   C-46 canonicalized identity binding, E7 shared storage) are retained as written for
   the future alias/re-activation case — they simply have one live key to bind today.
5. **E4 residual risk note:** single shipped name; the "ship both names" mitigation line
   is void. Typo-to-DNS risk unchanged.
6. **Migration simplification (§7):**
   - Step 2: republish keeps the **existing** embedded `ocx.sh/...` identifiers — the
     identifier-rewrite requirement is deleted; D6 ownership check matches current
     manifests as-is. (Republish itself still happens: packages move Artifactory → ghcr.)
   - Step 3: no default flip; ship baked map only.
   - Step 4: coworker cutover = binary upgrade + `ocx lock` re-resolve (E5). Bare
     `ocx.sh` names in ocx.toml/CI keep working verbatim.
   - Step 6: becomes "ocx.rs disposal": domain idles, renewal optional; Cloudflare zone
     kept or deleted at leisure.
7. **Endpoint bootstrap ordering (new ground-setup gate):** `index.ocx.sh` requires the
   `ocx.sh` zone on Cloudflare (nameserver change at registrar; inventory existing
   records first — mail, Hetzner A records for prod/dev/setup subdomains). Until then
   `index.ocx.rs` keeps serving as bootstrap endpoint. **Gate:** the baked map pointing
   at `index.ocx.sh` must not ship in a release before `index.ocx.sh` serves 200s. No
   client compat issue — no release has shipped any baked map yet.
8. **Hosting note:** website canonical host becomes `ocx.sh` on Cloudflare Pages
   (`ocx-website` project gains the `ocx.sh` apex custom domain after zone onboarding);
   `ocx.rs` apex + `index.ocx.rs` custom domains detach at parking time. GitHub org/repo
   names (`ocx-sh/index`) unchanged — org naming never appears in identifiers.

---

## Appendix A — Contract-to-Test Quick Index

Config: C-01…C-17 (unit, `config/registry.rs`). Rewrite/seam: C-18…C-25, C-20b, C-25b,
C-25c, C-41 (unit + integration, `registry_index/resolver.rs`, `app/context.rs`).
Dual-name/lock: C-26…C-31 (unit, `registry_index/resolved.rs`, `project/lock.rs`).
Client: C-32…C-50, C-53, C-54 (unit with stub transport, `registry_index/client.rs`;
C-42…C-44 cache). Errors: C-51, C-52 (unit, `classify_error`). Index-repo governance:
G-01…G-18 (`ocx-sh/index` CI; §11.8–13 for G-15…G-18 + G-09/G-13 amendments). Migration
gates M-1…M-3 (§11, ops checklist). End-to-end UX:
S1…S9 (acceptance, `test/tests/test_registry_indirection.py` against a local registry +
a stubbed/static index endpoint).
