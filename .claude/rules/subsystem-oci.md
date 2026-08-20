---
paths:
  - crates/ocx_lib/src/oci/**
  - external/rust-oci-client/**
---

# OCI Subsystem

OCI registry client, index management, identifiers, platform matching at `crates/ocx_lib/src/oci/`.

## The local index copy IS the package-tier lock (load-bearing)

Read this before changing anything under `oci/index/**`. Every routing rule below serves it.

`$OCX_HOME/index/` (or `--index`/`OCX_INDEX`) pins **both halves** of what a tag answers: tag →
digest, and logical → physical routing (a root's `repository` field). Three consequences:

1. **Determinism.** The same `ocx package install|exec <pkg>:<tag>` twice gives the same result —
   offline, and even when any online resource changed in between.
2. **A pin moves only under a command the user invoked naming what to move.** `ocx index update
   <pkg>...` moves the packages listed and nothing else. `ocx index sync <REGISTRY>...`
   moves the set the user named by naming the registries, by enumerating **each source's catalog at
   that instant** — an explicit operator act, never a default. Nothing else moves a pin: not a
   listing, not an update of a *different* package, and there is no implicit whole-index sync in any
   spelling. The one resolve that does move a pin is an explicit `--remote` one — it re-fetches and
   rewrites the tag it touches, the same write an `ocx index update` scoped to that tag would make;
   a **default**-mode resolve never does. `index sync` moves nothing under `oci/index/**` beyond tag
   pins, dispatch objects and the source's `config.json`; no patch-companion and no managed-config
   binding is recorded there, and nothing under `oci/index/**` gains the ability to record either.
   `ocx index regenerate` moves no pin at all — it re-derives
   `c/index.json` from the roots on disk.
3. **GC never changes identity.** `ocx clean` may evict blob CONTENT (refetched by digest,
   byte-stable); which digest a pin resolves to is out of its reach.

**Cross-tier routing follows from consequence 2.** A patch companion or a managed-config
source is never "named" the way consequence 2 means it — a companion is named by a
descriptor on the operator's behalf, and a managed-config source is not a package at all —
so neither pins here. They pin in their own tier-scoped state instead:
`state/patch-companions/` and `state/managed-config/snapshot.json`. Nothing under
`oci/index/**` may be asked to record either binding; see `subsystem-package-manager.md`
and `subsystem-file-structure.md` for where that state actually lives.

The tag pointer is not the only thing that would land here. A pinned `tag@digest` pull
skips `commit_root_tag` but still writes the **dispatch object** into the repository's
`o/`, so a companion install carries `LocalWritePolicy::ReadOnly` all the way through
(`PackageManager::read_only_view`) and the companion readers resolve their pin from
`$OCX_HOME/blobs` through a `ReadOnly` view rather than from `o/` — otherwise the
`AbsentDispatch` self-heal writes the object back on the first compose. A companion
repository owns **zero bytes** under `index/`.

The corollary is that `--frozen` does not reach them either. It freezes THIS pin, so a
companion resolve routes through `Index::remote_view` — mode-independent by construction —
rather than the ambient chain, whose `ChainMode::Frozen` would refuse an unpinned companion
tag that no local index was ever meant to hold (issue #293). `--offline` is a separate
question and is gated by the patch tier before any view is built.

**Silence principle.** If something is locally committed, resolving it makes no network request and
reveals nothing about remote state — no drift warning, no drift error, no comparison. Remote data
fetched during a genuine first-resolve contributes ONLY the missing entry; everything else
local-wins, silently. A resolve has nothing it may act on: it cannot take the update (violates 2)
and cannot refuse (the committed answer is the correct one), so a diagnostic there is noise on the
hottest path in the binary. Available updates surface exclusively through explicit staleness
reporting — `ocx index catalog --remote` — never through resolution.

Practical test for a change under `oci/index/**`: name the command the user ran, and the package
they named. If the diff can move a pin (a tag's `content`, or a root's `repository`) for anything
outside that set, it is wrong however well-motivated the fetch is.

ADR: `adr_index_indirection.md`, amendment "The local copy is the package-tier lock" (2026-08-05).

## Design Rationale

Trait dispatch (`IndexImpl`) swap local/remote index impls + inject test transports without changing callers. `OcxIndex`/`OciIndex` cache aggressive (RwLock per clone) avoid redundant registry calls in batch ops. `IndexImpl` methods return `Option` (None = not found) — absence normal query result at index layer, not error. See `arch-principles.md` for full pattern catalog.

## Module Map

| Path | Purpose |
|------|---------|
| `oci/index.rs` | Public `Index` wrapper; `ChainMode`; `SelectResult`; `fetch_candidates()`, `select()` |
| `oci/index/index_impl.rs` | Private `IndexImpl` async trait (11 methods — 6 required, 5 default-provided) |
| `oci/index/chained_index.rs` | `ChainedIndex`: cache + ordered sources + `ChainMode` routing |
| `oci/index/local_index.rs` | `LocalIndex`: owns the local index collection — wire-grammar, dispatch-object-only CAS (see "LocalIndex" below) |
| `oci/index/ocx_index.rs` | `OcxIndex`: remote client of a **published** ocx-index — root → dispatch object → `select_best` |
| `oci/index/oci_index.rs` | `OciIndex`: remote client that **derives** an index from a plain OCI registry's tags API |
| `oci/identifier.rs` | `Identifier`: parsed OCI reference with validation |
| `oci/digest.rs` | `Digest` enum: Sha256, Sha384, Sha512 |
| `oci/platform.rs` | `Platform`: os/arch matching, `any()` for platform-agnostic packages |
| `oci/client.rs` | `Client`: registry operations (list, fetch, push, pull) |
| `oci/ssrf.rs` | Default-on SSRF guard for remote-controlled hosts: `is_forbidden_ip`, `host_is_trusted`, `resolve_and_validate` (pre-flight), `GuardedResolver` (`reqwest::dns::Resolve` pin) |
| `oci/client/transport.rs` | `OciTransport` async trait (abstract HTTP transport) |
| `oci/client/native_transport.rs` | Native transport using `oci_client` library |
| `oci/client/hashing_reader.rs` | `HashingAsyncReader`: digest tee over sha256/sha384/sha512 |
| `oci/client/progress_reader.rs` | `ProgressReader`: cumulative download progress callback |
| `oci/referrer.rs` | Root module; re-exports `ReferrersApiCapability`, `ReferrersSupport`, `ReferrerManifest` |
| `oci/referrer/capability.rs` | `ReferrersApiCapability` probe + per-registry capability cache (`$OCX_HOME/state/referrers/<registry>.json`) |
| `oci/referrer/manifest.rs` | `ReferrerManifest` — OCI referrer manifest builder and descriptor helpers |
| `oci/referrer/media_types.rs` | Media-type constants for Sigstore bundle and other referrer artifact types |
| `oci/endpoint.rs` | Sigstore endpoint SSRF/scheme validation — `validate_sigstore_url` (the boundary for `--rekor-url`/`--fulcio-url`), the default public Rekor endpoint constant (`DEFAULT_REKOR_URL`), and the shared timeouted `sigstore_http_client()` reused by all four Sigstore call sites (Fulcio CSR, Rekor upload, ambient OIDC token exchange, verify pipeline), whose DNS resolver replays the guard's pinned addresses so no dial re-resolves a name the guard already judged. A peer of `oci/sign` and `oci/verify`, not owned by either |
| `oci/sign.rs` | Root module; re-exports signing public types |
| `oci/sign/bundle.rs` | Sigstore bundle v0.3 (`application/vnd.dev.sigstore.bundle.v0.3+json`) serialisation |
| `oci/sign/error.rs` | `SignError` + `SignErrorKind` — three-layer error with exit-code classification |
| `oci/sign/fulcio.rs` | Fulcio CA client — CSR construction + certificate issuance |
| `oci/sign/oidc.rs` | OIDC token provider trait + dispatch logic |
| `oci/sign/oidc_ambient.rs` | Ambient CI token detection (dispatches to inline fallback) |
| `oci/sign/oidc_ambient_inline.rs` | Per-platform inline ambient OIDC fetchers (GHA, GCP, etc.) |
| `oci/sign/oidc_browser.rs` | Interactive browser OAuth PKCE flow (suppressed with `--no-tty`) |
| `oci/sign/pipeline.rs` | `SignPipeline` orchestrator — resolve target, capability-check referrers, acquire OIDC token, delegate signing to a `Signer`, push bundle + referrer manifest. Wired end-to-end (#194) against a real Fulcio and Rekor (`sigstore` compose profile); Rekor SET + Merkle proof verification is delegated to `sigstore-rs` |
| `oci/sign/rekor.rs` | Rekor transparency-log client — log entry POST + SET extraction |
| `oci/sign/signer.rs` | `KeylessSigner` — ephemeral ECDSA P-256 keypair generation |
| `oci/verify.rs` | Root module; re-exports verification public types |
| `oci/verify/error.rs` | `VerifyError` + `VerifyErrorKind` — three-layer error with exit-code classification |
| `oci/verify/identity.rs` | Certificate identity + OIDC issuer exact-match policy |
| `oci/verify/pipeline.rs` | `VerifyPipeline` orchestrator — resolve target, list signature referrers (capability cache), parse bundle, verify cert chain + Rekor SET + signature + identity/issuer. Wired end-to-end (#194); with no override and no cache the trust root is fetched over Sigstore TUF |
| `oci/verify/trust_root.rs` | Trust-root loading: Sigstore `TrustedRoot` JSON (`--sigstore-trusted-root`, pinned Rekor key), cache rebuild; public-good root over TUF (`SigstoreTrustRoot`). There is no CA-PEM loader — a bare Fulcio CA carries no CT log key, so the pipeline refuses it |
| `oci/verify/trust_cache.rs` | Trust-root cache for offline verify (`$OCX_HOME/state/trust_root/<rekor-authority>.json`) — Fulcio CA + pinned Rekor key, atomic write, TTL, fail-open; mirrors `referrer/capability.rs` |
| `oci/verify/trust_resolve.rs` | `resolve_trust_root(explicit_override, sigstore, home_trusted_root, state, rekor_cache_key, offline)` — the shared six-rung ladder: `--sigstore-trusted-root` flag ▸ `OCX_SIGSTORE_TRUSTED_ROOT` (both arrive already collapsed) ▸ `[trust.sigstore] trusted_root`/`trusted_root_json` ▸ `$OCX_HOME/sigstore/trusted-root.json` ▸ trust-root cache ▸ public-good root over TUF. Rungs 1-3 are operator-named (a missing file is an error); rung 4 is a convention (absent falls through, unreadable-but-present fails). Single source of the offline pinned-Rekor-key gate for both the `ocx package verify` command (flag→override) and the auto-verify hook shared across every install surface (env→override) |

## Key Types

### ChainMode

```rust
pub enum ChainMode {
    Default,  // Local index first for queries. `Op::Resolve` walks chain on miss; `Op::Query` returns None. Normal online operation.
    Remote,   // Mutable lookups (tag list, catalog, tag-addressed manifest) hit source directly. Digest-addressed lookups still consult local index. Used for `--remote`.
    Offline,  // Local index only; source never consulted. Digest miss → None; unpinned-tag `Op::Resolve` miss → PolicyResolutionBlocked (exit 81). Used for `--offline`.
    Frozen,   // Freeze resolution to the local index: unpinned-tag `Op::Resolve` miss → PolicyResolutionBlocked (exit 81); digest-addressed content still walks the source like Default. Used for `--frozen`.
}
```

`ChainMode::policy_label(self) -> &'static str` returns the lowercase flag name (`"offline"` / `"frozen"`) embedded in the `PolicyResolutionBlocked` message.

**Deferred: composed RoutingPolicy.** A struct form (`RoutingPolicy { resolution: Allowed|LocalOnly, network: Allowed|Banned }`) was considered and deferred in favor of the flat `ChainMode` enum — YAGNI: only one policy axis exists today, and four variants enumerate the space exactly. Revisit trigger: when a *second orthogonal* policy flag appears, compose instead of adding a fifth flattened variant (combinatorial growth is the signal).

### IndexOperation

`IndexImpl::fetch_manifest{,_digest}` (and the `Index` wrapper's `select` / `fetch_candidates`) take an `IndexOperation` argument that declares caller intent:

```rust
#[non_exhaustive]
pub enum IndexOperation {
    Query,    // pure read; ChainedIndex returns None on miss, never walks the chain
    Resolve,  // install / pull; ChainedIndex walks the chain + persists on miss
}
```

The enum exists because the trait used to conflate query and update — a cache miss in `ChainedIndex::fetch_manifest` would silently walk the source chain and persist the result, leaking writes through query paths. Making intent explicit at every call site rules out that class of bug. See `adr_index_routing_semantics.md`.

### Routing matrix

| Operation | `--remote` | `--offline` | `--frozen` | `--offline --remote` | Default |
|-----------|-----------|-------------|------------|----------------------|---------|
| `list_repositories`, `list_tags`, `fetch_manifest` tag+`Op::Query` | source only, no write | local only | local only | local only (info log) | local only |
| `fetch_manifest` tag+`Op::Resolve` | source only, write blobs+tag | local only; unpinned miss → **PolicyBlocked (81)** | local only; unpinned miss → **PolicyBlocked (81)** | local only (→ 81) | local first, miss → fetch+write |
| `fetch_manifest` digest, any op | local first | local only | local first | local only | local first |
| `fetch_manifest` digest+`Op::Resolve` (pinned-id pull) | source on miss, write blobs only, **no tag** | local only | source on miss, write blobs only, **no tag** | local only | local first, miss → fetch blobs only |
| `physical_reference` (root `repository` pointer) | source first, local on miss or outage | local only (no sources) | local first, miss → source | local only | local first, miss → source |

**No-resolve policy block (offline + frozen).** Both `Offline` and `Frozen` refuse to resolve an unpinned (tag-only) reference from a source. The shared gate at the top of `ChainedIndex::walk_chain` is an exhaustive `match self.mode` — the `Offline | Frozen` arm with an `identifier.digest().is_none()` guard raises `oci::index::error::Error::PolicyResolutionBlocked { identifier, policy }` → `ExitCode::PolicyBlocked` (81); adding a new `ChainMode` variant forces a compile error at this routing decision. This is a deliberate behaviour change for offline: an unpinned-tag `Op::Resolve` miss now surfaces as `PolicyBlocked` (81), not `TagNotFound` (79) — realizing offline's documented "errors if missing" contract and aligning it with frozen. `TagNotFound` (79) now means strictly "a source *was* consulted and the tag genuinely does not exist" (Default / Remote). The two policies still differ on the digest axis: offline blocks the pinned digest's *content* fetch, frozen lets it through (only unpinned-tag *resolution* is refused). The project-lock layer mirrors this with `ProjectErrorKind::PolicyBlocked` (terminal, no retry).

**Update-family (lock-scoped) routing.** `ocx update` resolves Remote-style by default and **never persists tag pointers**: `Context::update_index()` builds `Index::from_chained_lock_scoped` (mode ladder `--offline` ▸ `--frozen` ▸ `Remote` — no `Default` arm), which sets `ChainedIndex.suppress_tag_commit`. The gate skips `commit_root_tag` in `fetch_and_persist_chain`; manifest blobs still persist (content-addressed). `walk_chain` returns the persisted chain's head digest so the suppressed tag-addressed `Resolve` read-back can address the blob by digest instead of the (deliberately absent) tag pointer. `--offline`/`--frozen` keep the `PolicyBlocked` (81) contract because everything stays `Op::Resolve`. ADR: `adr_toolchain_update_family.md`.

**Design note — write paths.** Local index mutation is owned by exactly four entry points: `LocalIndex::refresh_tags` (called from `ocx index update` and `ocx index sync`, both through the one shared fan-out in `command/index_common.rs`; grows a package's root + dispatch objects, A2/A3), `LocalIndex::persist_dispatch` (single dispatch-object write per chain fetch — never walks child manifests, A3), `LocalIndex::commit_root_tag` (`pub(super)`, the derived root-document tag writer outside `refresh_tags`), and `LocalIndex::commit_published_root` (`pub(super)`, its published counterpart — merges within a `RootScope`). Both root writers merge; neither replaces. `commit_root_tag` and `commit_published_root` are called from `ChainedIndex::fetch_and_persist_chain`, and `commit_published_root` additionally from `refresh_published`. Pure query paths must never reach any of them. The structural test `chain_refs_tests::op_query_never_writes_local_index_in_any_mode` enforces this for `Op::Query` (Default/Offline → `None`, no source; `--remote` → read-through to source via `query_sources_manifest{,_digest}`, returns `Some`, tag store untouched). Pinned-id pulls (`tag+digest`) skip the `commit_root_tag` step because `ocx.lock` is canonical.

**`LocalWritePolicy` — how much a `Resolve` may write.** `ChainedIndex` carries a `LocalWritePolicy` (replaces the former `suppress_tag_commit` bool), a descending ladder of local-index mutation independent of `ChainMode`:

| Policy | Constructor | Dispatch object (`o/`) | Root-doc tag pointer | AbsentDispatch self-heal |
|--------|-------------|------------------------|----------------------|----------------------|
| `Full` | `new` / `from_chained*` | write | grow | write |
| `NoTag` | `new_lock_scoped` (update family) | write | skip | write |
| `ReadOnly` | `read_only_view` (inspect, patch-companion pull + read) / `remote_view` (update check, companion resolve) | **skip** (`fetch_dispatch_only`, no stage) | skip | skip |

`ReadOnly` is the only policy that persists **nothing** into the permanent index — a read-only `ocx package inspect` resolves content-addressed (index → blobs → source) and warms the GC-able blob cache (`stage_leaf_manifest`, `stage_chain_blobs`, config-blob `fetch_blob` all still write `$OCX_HOME/blobs`), but never grows the committed index. Threaded as `IndexImpl::read_only_view` (default = `box_clone`; `ChainedIndex` flips the policy) → `Index::read_only_view` → `PackageManager::read_only_index` / `read_only_view`. The write policy survives `box_clone`. Acceptance: `test/tests/test_inspect_no_index_growth.py`; unit: `chain_refs_tests::read_only_view_resolves_but_writes_no_dispatch_object_or_tag_pointer`. Rationale: the index is deployment-managed and outside GC (`adr_index_indirection.md` B1), so an inspect-time write would be permanent pollution; the blob cache self-cleans on `ocx clean`.

**`remote_view` — the second `ReadOnly` view.** `IndexImpl::remote_view` (default = `box_clone`; `ChainedIndex` returns `read_only()` with `mode` flipped) → `Index::remote_view`: `ChainMode::Remote` **and** `LocalWritePolicy::ReadOnly` in one view. Consumer: the update-check probe (`TagProbe::Remote` in `package_manager/tasks/update_check.rs`), which must see the freshest *published* release regardless of the ambient ChainMode — `ocx.sh/ocx/cli` is a logical name the published index routes to a physical repository, so listing it through the chain rather than a registry's tags API is what makes the newest release visible at all. `ReadOnly` is stronger than the listing needs (a listing writes under no policy) and deliberately so: a look for an update must be incapable of moving a pin. Unit: `chain_refs_tests::remote_view_lists_from_source_under_default_mode_and_writes_nothing`.

### Identifier

Parsed OCI reference: `registry/repository[:tag][@digest]`.

- `parse_with_default_registry(s, default)` — main entry point
- `tag()` returns `Option<&str>` — does NOT inject "latest" (unlike `oci_spec::Reference`)
- `tag_or_latest()` — returns tag or "latest" fallback
- `clone_with_tag(tag)` — new identifier with tag, drops digest (tag change invalidates digest)
- Tags with `+` normalized to `_` on parse (OCI spec forbids `+`)
- Repository must be lowercase (validated on parse)

### Index (public wrapper)

Type-erased wrapper over `Box<dyn IndexImpl>`. Construction:
- `from_chained(cache: LocalIndex, sources: Vec<Index>, mode: ChainMode)` — standard constructor; wraps `ChainedIndex` orchestrating cache + source routing per `ChainMode`
- `from_remote(oci_index)` — wraps bare `OciIndex` (no caching)
- Clone shares in-memory cache (via `Arc<RwLock>`)

Key methods: `list_tags()`, `fetch_manifest()`, `fetch_candidates()`, `select(identifier, platforms) → SelectResult`

### SelectResult

```rust
#[non_exhaustive]
pub enum SelectResult {
    Found(Identifier),           // Exactly one match
    Ambiguous(Vec<Identifier>),  // Multiple matches
    NotFound,                    // No candidates (no os/arch match, or package absent)
    FeatureMismatch {            // os/arch present, but no candidate's os_features ⊆ host
        host_features: Vec<String>,
        available: Vec<Platform>,
    },
}
```

`FeatureMismatch` is distinct from `NotFound`: the package ships for this os/arch but only under `os.features` the host does not satisfy (e.g. a different libc). The package-manager layer maps it to `PackageErrorKind::FeatureMismatch` → `ExitCode::DataError` (65); `available` lists candidate platforms the user can `--platform`-override to.

### IndexImpl Trait (private)

```rust
async fn list_repositories(&self, registry: &str) -> Result<Vec<String>>;
async fn list_tags(&self, id: &Identifier) -> Result<Option<Vec<String>>>;
async fn fetch_manifest(&self, id: &Identifier, op: IndexOperation) -> Result<Option<(Digest, Manifest)>>;
async fn fetch_manifest_digest(&self, id: &Identifier, op: IndexOperation) -> Result<Option<Digest>>;
```

`list_tags` / `list_repositories` are query-only by definition and do **not** take `op`. `fetch_manifest{,_digest}` callers must pass `Op::Query` for pure reads or `Op::Resolve` for install/pull paths.

**Return convention**: `Result<Option<T>>` — `None` = not found (not error), `Err` = network/IO failure.

### LocalIndex — wire-grammar collection, dispatch-only

`LocalIndex` owns the local index **collection**, not a single index — one subtree per source
under a single home (`--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index`, `context.rs`). Each source's
subtree is the `index.ocx.sh`-hosted wire grammar verbatim: `config.json`, `c/index.json`,
`p/<ns>/<pkg>.json` root documents, `p/<ns>/<pkg>/o/<algo>/<hex>.json` dispatch objects —
no local re-encoding. Two provenance kinds share the grammar and diverge only in who authored the
bytes:

- **Published** (`index.ocx.sh` and mirrors of it) — bytes copied verbatim from the hosted site;
  self-verifying (object filenames are their own recomputed SHA-256; root documents verify against
  their `c/index.json` catalog entry).
- **Derived** (a plain OCI registry) — OCX authors the root document itself in the same grammar
  (`{ repository: "oci://<physical>", tags: { "<tag>": { content, observed } } }`); no
  `config.json`/`c/index.json`; catalog = directory enumeration of `p/`.

**Dispatch objects only — `o/` never holds a leaf manifest.** `o/` stores the OCI image index a tag
resolved to, verbatim — nothing else. A leaf platform manifest is content, fetched on demand into
the machine-global blob store, never copied here. Every tag's `content` names an image index present
in `o/`; a curated tag that resolves to a bare manifest is refused at announce time, not recorded
(`TagIsNotAnImageIndex`). Digest-addressed leaves (`pkg@sha256:…`) are content addressing, not
dispatch, and never touch `o/`.

**Absent-dispatch recovery from the blob store (A3 step 2).** `ChainedIndex::with_content_store`
(wired once in `context.rs` via `Index::from_chained_with_content_store`, passing
`file_structure.blobs`) tries `$OCX_HOME/blobs` **before** any source walk when a resolve hits
`DispatchResolution::AbsentDispatch`: an installed package's image index was cached there at install
time (`stage_and_link_chain_blobs`), so it resolves fully offline with zero network — the
"installed-tool offline exec is unaffected by A3" guarantee (B2). The read is digest-verified
(A4); a verify or decode failure is a clean miss (`Ok(None)`), never a hard error, falling through
to the ordinary source walk or offline/policy path. Recovered bytes are unconditionally an OCI
image index — there is no leaf outcome to disambiguate — and self-heal back into `o/` the same way
a source-fetched one does. `ChainedIndex::fetch_blob` shares this same content-store seam —
cache-first digest-verified read and, on a source fetch, the verified write-through against the
same attached `BlobStore`; `LocalIndex::fetch_blob` is an `Ok(None)` stub, the index home owns no
blob CAS of its own.

**Grow ≠ refresh.** `ChainedIndex::walk_chain`'s `grow_root` flag distinguishes the two miss
shapes a caller can observe locally before walking: a genuinely unknown root/tag (`true` — the
walk also grows the local copy) versus an already-known root whose dispatch object is merely
absent (`AbsentDispatch`, `false` — recovery only, the root is never re-copied). Invariant 1 (a
published root is never auto-refreshed under Default) holds because `grow_root` is only ever
`true` on a genuine first-time miss, never on an `AbsentDispatch` recovery of an already-present root.

**Merge is the only write verb — the local index is AUTHORED, not mirrored.** There is no
verbatim-replace writer anywhere: `LocalIndex::commit_published_root(identifier, bytes, scope)` is
the one published-root writer, and it merges within `RootScope`:

| Scope | Written by | Adopts | Leaves alone |
|---|---|---|---|
| `RootScope::Tag(t)` | `ocx index update pkg:t`, every grow-on-resolve | `tags[t]` | every sibling pin, `repository`, every package-level field |
| `RootScope::Package` | `ocx index update pkg` (bare), and `ocx index sync <REGISTRY>` for every package the registry's catalog names | every tag the remote lists + package-level fields (routing) | any tag only the local copy holds |

**Neither scope deletes.** A tag the remote stopped listing survives locally with its pinned
digest, both provenance kinds — `commit_root_tags` (derived) always upserted, and
`commit_published_root` now does too. The copy records what this machine snapshotted, so a
publisher retiring a version cannot break a machine pinned to it.

First sight is not an exception: with no committed root the merge runs against the fetched
document with its `tags` emptied, so a tagged first-resolve lands exactly the tag it resolved
(not the site's whole list) while the package-level fields come along — there is nothing yet to
protect. Committed bytes no reader accepts get the same treatment: recovering a crashed write is
not overwriting committed state. All of it silent — no diagnostic on whatever else the fetched
root has moved on to (silence principle). `refresh_published` scopes its dispatch-object persists
the same way: only the tags a write adopts need their objects, since unadopted tags keep the pins
— and therefore the objects — they already had.

**An `AbsentDispatch` recovery walks the PIN, not the tag.** The committed root already binds this
tag to `content`, so `fetch_manifest` addresses the walk with `identifier.clone_with_digest(content)`.
Addressing the tag would return the registry's current digest while the root still pinned the old
one — the same tag answering two digests depending on whether a cache was warm. Its sibling
`fetch_manifest_digest` short-circuits from the same pin with zero network; the digest-addressed
walk is what keeps the two answers identical. Reachable for a bare-leaf tag (single-platform; `o/`
never holds a dispatch object for it by design) whose blob was evicted, and for an image index
missing from a partial copy.

**A fetched leaf manifest warms the blob store.** `fetch_and_persist_chain` writes a
`Manifest::Image` result's verbatim bytes to the attached `content_store` under the LOGICAL
registry + digest — the same key `recover_absent_dispatch` reads — so the next resolve of that pin
needs no network. Best-effort (a write failure is `debug!`, never fails the resolve). An image
index is not written there: it is already in `o/`. GC treats it as a cache; an installed package
roots it through its own `refs/blobs/`, and an evicted copy refetches by digest, byte-identical.

**Authoritative-stop, no silent fallthrough.** When the one configured source for a namespace
(Decision H — exactly one remote per namespace) is authoritative for an identifier and errors
(yanked tag, dispatch-object tamper, fail-closed unknown `config.json` version, network failure), the walk
returns that error immediately rather than falling through to a "not found" result — a broken or
misconfigured `[registries."<ns>"] index` endpoint fails loud, never silently resolves as absent.
Its clean miss is equally terminal.

Every source loop honours it, not just the resolve walk: `fetch_and_persist_chain`,
`query_sources_manifest{,_digest}`, and the Remote-mode `list_tags` loop all pair each source with
the `candidate_sources` authority flag and stop on it. Tag listing is not exempt — a fall-through
there answers for the LITERAL name off the registry catch-all, which for an indirected package is a
stale, capped tag list that reads as a confident answer (the v0.5.0 self-update always-up-to-date
bug). An authoritative listing failure propagates instead, and the update-check caller maps it to
`Skipped(RegistryProbeFailed)` → exit 75.

*Scope of the yank half.* The published yank gate lives on the **tag** path
(`OcxIndex::fetch_manifest_raw_bytes` → `surface_status`); a digest-addressed fetch skips it by
design, because a yank is a tag-lane publisher signal and an immutable pin is not a tag. So a
recovery walk addressed by the committed `content` digest — the AbsentDispatch path above — does
not re-ask the published root and does not re-surface a yank published since the pin was taken.
A yank **already recorded in the committed root** still enforces: `resolve_dispatch` runs
`surface_root_status` on the tag read before any walk happens, so the refusal fires locally with
zero network. Whether a resolve *should* re-consult the source for a yank published after the pin
is an open semantics question (it trades against the silence principle and against invariant 2) —
this paragraph describes what happens today and decides nothing.

**Index-declared jurisdiction (`Jurisdiction`, `oci/index.rs`).** Whether a source is asked at all is
a *three*-valued question, asked before any fetch — never an `Ok(None)` read after one:

| Verdict | Meaning |
|---|---|
| `Authoritative` | Ask it; its miss or refusal is terminal (the stop above). |
| `FallThrough` | Ask it; its miss tries the next source (the `OciIndex` catch-all). |
| `Outside` | It **declared** it cannot express this name — never asked, silence decides nothing. |

`OcxIndex::jurisdiction` (async, inherent `pub`, trait impl forwards) decides on **evidence**; the
published declaration (`name_segments` — `index.ocx.sh` serves `2`, restating its root schema's
`^ocx\.sh/<ns>/<pkg>$`) only ever interprets a *miss*:

| Case | Verdict |
|---|---|
| Foreign registry | `Outside`, **no I/O** (runs first) |
| No declaration, or a name it satisfies | `Authoritative`, no root fetch |
| Declared inexpressible, root **found** | `Authoritative` — the root overrules the declaration |
| Declared inexpressible, root **404** | `Outside` — any single-segment `ocx.sh/<tool>`, now that the fleet and this repo's own toolchain are two-segment |
| Declared inexpressible, root fetch **errors** | `Authoritative`, fail-closed |

The declaration must never stop the client *asking*: it is an unsigned, CDN-cacheable integer on the
same channel as the roots, so a template bug, a stale edge or a compromise scoped to that one file
would otherwise narrow a namespace and skip the yank gate on a name the index does hold a root for.
Because the root decides whenever it exists, a wrong `name_segments` can bypass nothing. Absent
`name_segments` = serves every name = historical behaviour verbatim, so a private index is never
narrowed by the client. **Fail-closed** throughout: a 404, malformed, unsupported or unreachable
`config.json`, or a root fetch that errors, keeps the source `Authoritative` (an index outage must
never downgrade a namespace to plain OCI). Infallible, not error-swallowing — nothing is cached on
failure, so the immediately-following `resolve_root` re-fetches and raises the real error.

Only a **confirmed 404** may settle a declined name as `Outside`. `resolve_root` sends the root `GET`
unconditionally, so a `304` answering it is a misbehaving edge, not an absence — it raises
`IndexHttpFailed`, and an errored root keeps the source `Authoritative` like any other fetch failure.

Cost: a declined name adds exactly one root `GET` that 404s. `SourceCacheInner.roots` memoizes the
miss (`Option<Arc<IndexRoot>>`) alongside the hit, so it is one request per name per process however
many times the chain consults jurisdiction — eight flat tools in a project = eight cached 404s on a
cold run. The `Outside` verdict logs at **`debug!`**: it is the steady state for the shipped namespace
(41 of 44 `ocx.sh` repositories are flat names), and the memo suppresses the request, not the log line
— `jurisdiction` is re-entered from every `candidate_sources` call, so at `warn!` one cold `ocx lock`
emits tens of identical lines.

`ChainedIndex::candidate_sources(id) -> Vec<(&Index, bool)>` is the one place the question is asked;
the paired bool drives every terminal-stop arm. No client-side name rule exists anywhere — the
declaration is the index operator's, and `name_segments` is **not** a security control (an older
client ignores it; the yank gate, dispatch-object digest verify and terminal stop delegate nothing to it).

**Provenance is per REGISTRY, jurisdiction per NAME.** `IndexImpl::serves_registry(&str)` (sync,
no I/O) is the ownership primitive behind `ChainedIndex::kind_for_registry`; `kind_for(id)` delegates
with `id.registry()`. Local subtree layout (`c/index.json` catalog vs `p/` enumeration) is per-source,
never per-name, so every name under a published registry reports `Published` regardless of grammar.
Deriving provenance from a per-name predicate would flip a declined name to `Derived` and silently
drop the published root's catalog cross-check. There is no placeholder identifier anywhere.

**There is no *implicit* catalog sync.** `ocx index update <pkg>...` fetches the named packages'
roots and nothing else. `ocx index sync <REGISTRY>...` reads each source's catalog **to choose the
set**, then performs exactly the same per-package work through the same bounded loop. `ocx index regenerate` fetches nothing: it
rebuilds the local catalog from the local `p/` tree. Neither form reports on a package it was not
asked about.

**The local `c/index.json` is AUTHORED, not mirrored.** Its entries are `sha256(local root bytes)`,
written by `CatalogTransaction::write_root` — the only writer — in the same transaction that writes
the root. Nothing is ever persisted from a remote catalog, so the root/catalog straddle that the
old sync's keep-the-stale-row logic existed to avoid cannot arise. `ocx index regenerate` is the
operation that restores the catalog to a pure derivation of `p/` when it has drifted — re-deriving
every entry from the roots on disk rather than writing from a fetch — which is the only way an entry
naming a removed root is ever cleared.
`OcxIndex::fetch_catalog` reads the site's listing live and persists nothing; it exists for
`ocx index catalog --remote`.

Consequence for listing: default-mode `ocx index catalog` lists what this machine has materialized
(the catalog keys are exactly the roots on disk); `--remote` asks the site. That is the intended
split — default is the index you maintain, `--remote` is the site's answer.

"Am I behind?" is deliberately a question about the remote, asked explicitly (`ocx index catalog
--remote`, `ocx index list --remote`), never answered from locally recorded shadow state: every
storage option for a last-observed-remote digest re-introduces mirroring one field at a time.
**No per-machine bookkeeping in the tree.** The local tree is a distributable artifact (A2), so
nothing lands in it that is neither served content nor authored from it — which is why the `.etag`
sidecar was removed and why no last-observed-remote digest replaced it.
`CatalogTransaction::commit` writes nothing when the merged map equals the one it read, so a
no-op leaves the tree byte- and mtime-identical, and it opportunistically deletes a
`c/index.json.etag` left by an older ocx.

`LocalIndex::refresh_tags` stays jurisdiction-unaware: the shared refresh loop
(`command/index_common.rs`, behind both `ocx index update` and `ocx index sync`) picks the source
that will answer for each package before calling it, and reroutes a name the index declines to the
registry.

**Write path.** Raw response bytes are kept verbatim — no `serde_json::to_vec_pretty`
re-serialization. The digest is recomputed from those bytes and verified against the
source-claimed digest before the write commits (`DataError` on mismatch, CWE-345 class).
`ocx index update <pkg>` and `ocx index sync <REGISTRY>` write per tag in a fixed order — dispatch
object into `o/` → root document (atomic rename) → catalog entry (atomic rename) — each step
idempotent; a crash between any two steps recovers on the next read/update. The catalog entry is
exactly `sha256(root bytes)`: a read-time mismatch is an **inconsistency**, recovered by
re-derivation (info/debug log), never a hard error. `DataError` is reserved for genuine corruption
recomputation cannot fix — an unparseable root, a dispatch object whose bytes disagree with its own
`o/` filename, a failed `repository` cross-check. A later catalog sync finding a *different remote* digest for an
already-snapshotted package reports staleness ("update available"), never an error.

**Version choice, not lock offline-ness.** `LocalIndex` resolves a *version choice* — a tag, a
devcontainer parameter — to a concrete platform-manifest digest with zero network. It plays no
part in `ocx.lock` resolution: a lock already stores the pinned platform-manifest leaf digest
directly and fetches it content-addressed without ever consulting an index
(`adr_platform_model_unification.md` D3). Redirection (`--index`/`OCX_INDEX`) exists so a
*shipped* copy (a devcontainer feature, a CI artifact, a repo-committed directory —
conventionally `.ocx/`, never required) resolves free version choice deterministically with no
machine-global dependence — not to make `ocx.lock` resolve offline, which it already does without
any index.

**Outside GC.** `LocalIndex` is user/deployment-managed data, never walked by GC — `ocx clean`
inspects only `packages/layers/blobs`, so a machine-local clean can never collect a shipped copy's
data. ADR: `adr_index_indirection.md` Decision A, B1.

**Snapshot exemption.** A dispatch object's `content`-pointer digest (an image-index digest, for
either source kind) is exempt from the platform-manifest-only lock doctrine
(`adr_platform_model_unification.md` D3) because its bytes
travel *with* the pointer in the same `o/` — no later re-resolvable fetch exists for the doctrine
to protect against. `adr_index_indirection.md` Decision D.

### Index component model

| Component | Role |
|---|---|
| `LocalIndex` | Owns the collection above. Both provenance kinds go through it — the only divergence is catalog source (file vs directory enumeration) — dispatch decode is unconditional, one OCI parse for both provenance kinds. Filesystem mechanics (tempfile+rename CAS, the self-heal write) are an internal, non-headline detail. |
| `OcxIndex` | Remote client of a **published** ocx-index — root → dispatch object → `select_best`. |
| `OciIndex` | Remote client that **derives** an index from a plain OCI registry's tags API. |
| `ChainedIndex` | `LocalIndex` ▸ **exactly one** remote per namespace, chosen by config (`[registries."<ns>"] index` field presence), never probed. |

ADR: `adr_index_indirection.md` Decision H.

### index.ocx.sh source

`index.ocx.sh` is a pointer index, not a registry — no `/v2`, no blobs. Resolution pipeline (Decision C):

```
logical id → index resolve (root p/<ns>/<pkg>.json → tags[tag].content → index digest, verified)
           → GET p/<ns>/<pkg>/o/<algo>/<hex>.json → manifests[] : select_best(host, [(Platform, Digest)])
           → physical (root.repository, e.g. "oci://ghcr.io/…") → mirror_map → fetch (OCI CAS verify)
```

The OCI image-index hop is **served by the index instead of the registry** — not skipped, relocated.
Hop count is unchanged. `repository` is transport-only input (`oci://`
scheme, index-side wire contract) and never round-trips into a storage path — mirrors the `[mirrors]`
seam precedent (`Client::transport_reference`). Storage paths, `ocx.lock`, and GC roots key on the
**logical** identifier only (the `<source>` path segment), so a registry migration never orphans a
local copy or breaks a committed lock. There is no local-only scheme marker — source kind comes
from the path segment and the config registry kind, never a `repository`-field scheme.

`[registries."<ns>"] index` (config) selects the ocx-index protocol for a namespace — field
presence is the kind marker, no probing, one protocol per namespace. `[mirrors]` splits into
`registry`/`index` roles keyed by traffic host: the `index` role overrides the base URL for
root/dispatch-object/`c/index.json` fetches, the `registry` role covers OCI distribution traffic, independently.
Both replace semantics, no egress fallback, resolved once at the client seam (`resolve_base_url`
for the index role, `Client::transport_reference` for the registry role). ADR:
`adr_index_indirection.md` Decision C, F.

### Platform

- `Platform::any()` — platform-agnostic packages (Java, text tools)
- `Platform::current()` — auto-detect OS/arch; populates `os_features` from `HostCapabilities::detect()` cached at context init
- `is_compatible(required, offered)` (free fn) — the one directed compatibility relation: `os`+`arch` equal, `variant` is offer-gated strict equality (unconstrained when the offer leaves it `None`), `offered.os_features ⊆ required.os_features` (inverted subset — a feature on the offer names a capability it demands of the host). An `Any` offer satisfies every requirement; an `Any` requirement is satisfied only by an `Any` offer. `compatibility_score(offered)` ranks matches lexicographically `(is_specific, matched_refinement_count, matched_os_feature_count)` — the middle axis counts a declared-and-matched `variant`. `select_best(required, candidates)` is the **one shared helper** — `Index::select` (fresh resolve), `project::lookup_host_leaf` (lock-read), and authoring `resolve_for_specific` (dependency pinning) all route through it, so the three answer identically for the same required platform and candidate set. ADR: `adr_platform_model_unification.md` D1.
- `Platform::Specific.os_features` — `Vec<String>` carrying `libc.*` tags (e.g. `["libc.glibc"]`); empty `Vec` means no libc requirement declared
- `Platform::with_os_feature(feature)` — **replace, never union**: every existing tag in `feature`'s namespace (the text before its first `.`) is dropped and `feature` inserted, so `libc.glibc` evicts `libc.musl` but leaves `gpu.cuda`. Union would emit `libc.glibc,libc.musl`, which subset matching ANDs into a platform no single-libc host resolves — unresolvable while looking authoritative. A **dotless** tag has no namespace and is never evicted (a declared bare `libc` survives `libc.glibc`), and `Any` is returned unchanged. Consumer: `libc_lint`'s paste-ready `--platform` suggestion, which is paste-ready precisely because `Display` sorts+dedupes and `FromStr` round-trips the result.
- `Display` is the single canonical, injective, round-tripping grammar — `os/arch[/variant][+feature[,feature...]]` (features sorted+deduped), `any` with no fields — shared by the `--platform` arg, every `ocx.lock` `[tool.platforms]` key, and every dependency pin-map key; round-trips via `FromStr`. Filesystem paths use `segments()`/`ascii_segments()`, not `Display`. ADR D2.
- Supported: linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64
- One platform per `ocx package create`/`push` invocation (D5) — no bundle-level target-platform set. `--platform` is single-valued everywhere except `ocx patch sync`'s bare-invocation fan-out over the concrete ship matrix (an explicit enumeration, not a selection — D4's one sanctioned exception).

### libc Differentiation

OCX encodes libc family in the OCI `platform.os.features` field using the `libc.*` namespace (`libc.glibc`, `libc.musl`). At install time `Platform::current()` discovers and identifies the host's dynamic loaders (discovery-then-identify; see Host detection below), populates `os_features`, and `Index::select` applies [`is_compatible`](#platform) to pick the manifest whose declared `os_features` are a subset of the host's.

**ADR:** `adr_platform_libc_os_features.md` (subset semantics, `libc.*` namespace, host detection); `adr_platform_model_unification.md` D1 (the relation itself, generalized beyond libc)

- `offered.os_features` empty → matches every host (static-musl / scripts / bare-manifest fallback).
- `offered` is `Platform::Any` → always matches (scripts, JARs, data bundles).
- `required` is `Platform::Any` (detection failed entirely) → never matches a `Specific` offer.
- `os` + `arch` equality required before the subset check runs; `variant` mismatch (when `offered` declares it) → no match.

**Invariant — subset scope is narrowly `os_features` only:**

Subset semantics apply **only** to `os_features`. Extending subset or any non-equality semantics to `variant` or `features` requires a new ADR — this matcher's narrow shape is load-bearing for predictable single-pass index resolution.

**Wire format normalization:**

`From<&Platform> for native::Platform` sorts and deduplicates `os_features` before emitting to JSON. Cascade eviction at `oci/client.rs` compares platform entries by `native::Platform` struct equality (positional `Vec`); unnormalized arrays would silently duplicate entries in the image index.

**Host detection:**

`HostCapabilities::detect()` (module `oci/host_capabilities.rs`) uses **discovery-then-identify**. *Discovery* builds a deduped candidate-loader set from three sources (priority order): (1) the `PT_INTERP` of a fixed system-binary allowlist (`INTERP_PROBE_BINARIES`, read via the `elf` crate) — the host's exact native loader wherever it lives, so non-FHS hosts (NixOS `/nix/store`, Gentoo Prefix, custom sysroots) resolve; (2) an arch-filtered scan of canonical loader dirs (+ immediate multiarch subdirs); (3) the hardcoded `GLIBC_LOADERS`/`MUSL_LOADERS` allowlist (fallback). *Identification* then classifies each loader **purely by its `--version` banner** (table-driven over `LIBC_FAMILIES`), unioning every positive into a sorted `BTreeSet<LibcFlavor>` via a concurrent `JoinSet` (no first-wins; deterministic by construction). A host may advertise multiple families (e.g. glibc + musl on Ubuntu + `musl-tools`), giving `os_features = ["libc.glibc","libc.musl"]`, so `is_compatible` admits both a `libc.glibc` and a `libc.musl` offer. Detection is Linux-only; macOS and Windows return an empty set without spawning subprocesses. Banner classification (`GNU libc`/`GLIBC` → glibc; Ubuntu 20.04 quirk: exit 127 → `{loader} /bin/true`; `musl libc` → musl, exit status ignored) makes the **gcompat → musl** case fall out by construction — the gcompat stub at the glibc path prints the musl banner, classified musl (identity, not equivalence; no special-case exclusion). Empty set → empty `os_features`; subset matching then accepts only entries with empty `os_features`. A minimal host with no readable loader yields the empty set (debug-logged when `/nix` exists). **Known limitation:** detect-env ≠ exec-env (distrobox/container/install-here-run-there) — deferred to a future ADR. Research: `.claude/artifacts/research_libc_detection_robustness.md` (v2), `research_libc_detection_methods.md` (v1).

**RESERVED `features` field:**

`Platform::Specific.features` (OCI v1.1.1 RESERVED field) is never serialized. Inbound values from foreign manifests are warn-and-dropped.

### Manifest Types

- `Manifest::Image` — single platform; `fetch_candidates()` returns one entry with `Platform::any()`
- `Manifest::ImageIndex` — multi-platform; one entry per child manifest with platform annotation

## Invariants

1. **Cache never invalidated** — both index types cache aggressive in memory. For fresh data, create new instance or call `update()`.
2. **Internal tags filtered** — tags prefixed `__ocx.` stripped by every `IndexImpl::list_tags()` auto.
3. **Digest overrides tag** — when identifier has both, `fetch_manifest()` uses digest direct.
4. **Auth at Client level** — index impls don't handle auth; `Client::ensure_auth()` called before operations.
5. **A read that backs a write shares the write's addressing** — reads are mirror-aware by default (`transport_reference`), writes are always canonical. Any read whose answer decides, gates, or verifies a write must ask for `ReadAddressing::Canonical` (the `*_addressed` variants on `Client`) so the whole transaction stays on one host; deciding from a mirror and applying to the canonical registry is CWE-345/367. Precedent: `merge_platform_into_index` (one canonical ref for read + write), `ocx package cascade check|repair`.

## Pull Path (streaming single-pass pipeline) {#pull-path}

`Client::pull_layer` assembles a single-pass pipeline per layer:

```
transport.pull_blob_streaming → .take(layer.size) → HashingAsyncReader(algo)
  → ProgressReader → XzDecoder/GzDecoder → SyncIoBridge → tar::Archive::unpack()
```

**Drain before finalize.** `tar`'s iterator stops at the end-of-archive marker and hands the
reader back undrained, so the codec trailer (gzip's 8-byte CRC+ISIZE footer, xz's index +
footer) and any post-terminator padding are typically still unread. `pull_layer` drains the
**buffered-compressed** level (below the decoder — a decoder can report decoded-EOF without
consuming its own trailing bytes) into `io::sink` before unwinding to `finalize()`. Without
it the digest covers only the prefix tar happened to demand, and whether the trailer rode the
last buffer fill is decided by network segmentation — a non-deterministic `DigestMismatch`.
The drain runs on the extraction-**error** path too — load-bearing, since the digest is what
attributes a format error to the registry rather than to a local archive problem. It is skipped
only when the decompressed cap was hit, where the caller returns `DecompressionCapExceeded`
without ever consulting the digest and draining would just pull the rest of a known bomb's
declared bytes into the sink. Bounded by `take(layer.size)`; swallows its own error, because the
checks below judge the outcome.

Post-drain checks, in order — cap → completeness → digest → extraction error:

| Condition | Error | Meaning |
|---|---|---|
| `bytes_read != layer.size` | `ShortBlobRead` (exit 75, TempFail) | Incomplete delivery — transport truncation or an ocx-side short read. Retryable. |
| digest ≠ descriptor digest | `DigestMismatch` (exit 65) | The registry served wrong content (CWE-345). |

Completeness is checked **first, and must stay first**: a prefix cannot hash to the whole, so
every incomplete delivery also fails the digest check and would otherwise be reported as a
registry fault — making an ocx defect indistinguishable from a supply-chain attack in both
directions. `DigestMismatch` is still surfaced before any extraction error: wrong bytes
(CWE-345) cause a tar format error, but the mismatch is the security-relevant attribution.

`NativeTransport::pull_blob_streaming` calls the fork's public `pull_blob_stream`, which
wraps the response in `VerifyingStream` (mismatch → `io::Error(DigestError::VerificationError)`
at stream end). `HashingAsyncReader` is canonical and covers all paths including
`StubTransport`; `VerifyingStream` is secondary.

**Decompression-bomb caps (CWE-400):**

| Cap | Limit | Applied to |
|----|-------|-----------|
| Compressed | `layer.size` bytes via `.take()` | Raw stream, before `HashingAsyncReader` |
| Decompressed | `max(256 MiB, 100 × layer.size)` | `SyncIoBridge` output inside `spawn_blocking` |

Exceeding the compressed cap is caught by the digest check. Exceeding the
decompressed cap returns `ClientError::DecompressionCapExceeded` (detected via a
`take(cap + 1)` probe byte, checked before the digest comparison) — never a
misattributed `DigestMismatch`. The decompressed cap is computed in `pull_layer`
and passed to the private `pull_layer_with_caps`, so tests can inject a small
ceiling without fabricating a huge archive. A descriptor `size` of zero or one
that does not fit `u64` is rejected up front as `InvalidManifest`.

No blob file is written to disk during pull — there is no `DropFile` guard to drop.

**`SyncIoBridge` occupancy:** `spawn_blocking` thread is held for the full
download + extract duration (previously extract only). At 10 Mbps × 200 MB ≈ 160 s.
Tokio blocking pool cap is 512. Deferred: add semaphore if install parallelism grows
unbounded. Note: `SyncIoBridge` uses `Handle::block_on` per read (not `block_in_place`);
creating it inside the closure is idiomatic (tokio issue #6795).

## Per-Layer Layout {#per-layer-layout}

`oci/layer_layout.rs` is the read/write boundary for optional per-layer strip + output
prefix. Manifest layer descriptors carry it as `annotations` keys `sh.ocx.layer.strip-components`
/ `sh.ocx.layer.prefix` (`oci/annotations.rs`), set only when a publisher supplies a
`<ref>:strip=N,prefix=P` layer-ref (`publisher/layer_ref.rs`) — a default push writes no
annotations, so manifests stay byte-identical. `resolve_layer_placement(annotations,
bundle_default)` resolves the fallback chain (`annotation → Bundle.strip_components → 0`)
into a `utility::fs::LayerPlacement`, called from `pull.rs` before
`assemble_from_layers_with_layouts` — the boundary exists so `utility/fs` never depends on
`oci` (DIP).

## Gotchas {#gotchas}

- **OCI tags mutable.** Never assume tag "frozen" or "pinned." Only digests immutable.
- **`Platform::can_run` deleted.** Superseded by `is_compatible`/`select_best` (D1) at every real call site; its unit tests were either redundant with `is_compatible_truth_table` or ported into it.
- **Cache coherence issue**: Some commands call `context.remote_client()` directly instead of going through `default_index`. Bypasses cache, produces inconsistent results. All index ops should route through `default_index`.
- **SSRF guard is default-on (`oci/ssrf.rs`, ocx#218).** An index root's `repository` pointer is remote-controlled data; `OcxIndex::physical_identifier` runs `resolve_and_validate` on the physical host **before** the first `oci::Client` fetch (X3 ordering) and the physical-fetch client is built with a `GuardedResolver` (`ClientBuilder::ssrf_guard`) so the connect pins the validated address (resolve → validate → pin, no DNS-rebinding window). The escape hatch is per-namespace `[registries."<ns>"].trusted_hosts` (exact host or CIDR) — never inferred from `[mirrors]`, `system_locked` applies. Refusal → `SsrfError` → `ConfigError` (78). Host *allowlisting* stays index-side governance; the client only enforces the private/loopback/link-local/metadata floor.
- **`physical_reference` is local-first under `Default`/`Frozen`/`Offline`, source-first only under `Remote`.** It reads a *pointer* into remote-controlled data (a copied index tree is a supported distribution mechanism, A2), not digest-verified content, and the layer pull that consumes it runs on the shared, unguarded `PackageManager` client — so what makes local-first sound is that the local answer carries its own SSRF floor (`guard_local_physical`, same `trusted_hosts`), not that the committed root is trusted. Asking the sources first cost a `GET <base>/config.json` (jurisdiction) + `GET <base>/p/<ns>/<pkg>.json` (root) **per invocation of every identifier-resolving command** — install, exec, which, add, lock, pull, run, env — to re-derive a pointer the committed root already carried; the old justification ("the same resolve already memoized that root") is false precisely when it matters, because `fetch_manifest` is local-first and a warm resolve never populates the source memo. This realizes `adr_index_indirection.md`'s own "snapshot-first under Default, a Default-mode resolve never reaches upstream for the root". `Remote` keeps source-first: there the tag resolution really did go to the source, and the update family wants the **live** pointer. The source walk (reached on a local miss, or first under `Remote`) holds a **transport outage** (`is_source_outage` — `IndexHttpFailed` only, a closed set so a new error class fails closed) and re-raises it when no local root answers; any other source error is a **refusal** and propagates immediately — an `SsrfError` answered around by the local root would be a guard that fires and is then discarded. Symmetrically, under the local-first modes a **local** guard refusal propagates without consulting a source (`Default`/`Frozen`; under `Offline` the guard now runs too, so a refusal propagates there as well) — under `Remote` the local read runs after the source walk, so the question does not arise there. The local read runs exactly once per call: after a local-first miss the walk's outcome is final. **There is no `ChainMode::Offline` early return**: ahead of the local read it would return `Ok(None)`, which `resolve_transport_pinned` reads as "no rewrite" and turns into the logical identifier; after it, it is unreachable.
- **The local answer is the ordinary answer, not a fallback.** It always was for a flat `ocx.sh/<tool>` name — `candidate_sources` skips a source declaring `Jurisdiction::Outside` and the live `index.ocx.sh` declares `Outside` for every flat name (41 of 44 fleet repositories), so the source loop never executed for those — and under the local-first order it is now the answer for indirected names too. That makes the committed tree the **authority** for the pointer on the warm path: pointer *provenance* is delegated to the local tree deliberately, as trusted deployment-managed input in the same trust domain as `$OCX_HOME` (`adr_index_indirection.md` A2), and the SSRF floor bounds what that authority may name rather than standing in for the provenance it is trusted with. The two trust senses are distinct: the *bytes* are remote-authored and stay untrusted (hence the floor), while their *placement* into the tree is the operator's act and is what the delegation trusts. `ChainedIndex::guard_local_physical` applies the SSRF floor to it with the **same** `trusted_hosts` set `OcxIndex` uses (`IndexImpl::trusted_hosts`, keyed on registry ownership — one config value, never a second notion). One carve-out only: a root naming the identifier's own registry is not a *rewrite* (every derived root is that shape; guarding it would refuse private registries that predate indices and whose namespace carries no source to hold an exemption). `ChainMode::Offline` was a second carve-out and is not any more — its premise ("offline builds no OCI client, and no sources, so `trusted_hosts` is unreachable") died when `context.rs` began building the registry client in every mode for offline `ocx package verify`. The exemption reaches the guard through `LocalIndex::with_trusted_hosts` (config → local index, wired once in `context.rs` beside `with_allow_yanked`), so it is available with zero sources; `ChainedIndex::trusted_hosts_for` reads the local index first and falls back to the owning source. Riding on the local index is what makes every chain built from it — default, lock-scoped `ocx update`, `ocx patch test`'s scratch chain, `PackageManager::offline_view` — judge a local answer against the same exemption; an air-gapped deployment on a private mirror declares `[registries."<ns>"].trusted_hosts` exactly as online. A DNS **lookup failure** on a genuine DNS name is tolerated — that is the steady state of the warm-store-no-network case, and refusing there re-raises the exit-69 bug — but an address-shaped host that fails to resolve (a bracketed IPv6 authority) stays fail-closed.
- **A rewritten physical target is re-validated at the dial site**, by `Index::guard_physical_dial`, before the first request uses it — the resolve-time tolerance above admits a host the guard could not judge, and the pull then dials it on the shared client after its own independent lookup, so a hostile local tree answering NXDOMAIN at check time and loopback at dial time would otherwise walk straight through. The dial-site half **fails closed on everything, a lookup failure included** (a connect can no more succeed than the lookup did; an answer appearing only between the two questions is the attack), shares the not-a-rewrite carve-out and the one `trusted_hosts` set (`IndexImpl::trusted_hosts_for`, keyed on the LOGICAL registry), and is memoized per pull operation in `extract_layers` so it runs once per pull and only when a layer is genuinely missing — a fully-warm operation never resolves the host. Residual: the shared pull client still has no `GuardedResolver`, so the narrower **validate→connect** rebinding window stays open; closing it needs a per-namespace resolver on that client.
- **`GuardedResolver` needs the fork's `ClientConfig::dns_resolver` seam.** The pin is realized by the vendored fork's injectable `dns_resolver: Option<Arc<dyn reqwest::dns::Resolve>>` on `ClientConfig`; `oci::Client`'s default reqwest client has no such hook. `ClientBuilder::ssrf_guard(trusted_hosts)` is the only production setter.
- **Submodule at `external/rust-oci-client/`** patched fork. Changes need upstream PRs. Only format new code (upstream uses 100-char rustfmt).
- **When unsure about current `oci-client` API**, query Context7 MCP (`mcp__context7__resolve-library-id` → `mcp__context7__get-library-docs`) before guessing. Upstream crate evolves independently of patched fork; training-data knowledge of API shape decays fast.

## Quality Gate

During review-fix loops, run `task rust:verify` — not full `task verify`.
Full `task verify` is final gate before commit.