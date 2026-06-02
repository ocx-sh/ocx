# ADR: Client-Declared OCI Registry Mirror (Source Replacement)

<!--
Architecture Decision Record — MADR format.
Owner: Architect (/architect). Handoff: Builder (/builder), Security Auditor (/security-auditor), QA (/qa-engineer).
-->

## Metadata

**Status:** Proposed
**Date:** 2026-06-02
**Deciders:** @michael-herwig, architect
**GitHub Issue:** #122 (this ADR) — implements toward #123, part of milestone #111 (Corporate infrastructure v1)
**Related PRD:** N/A (milestone #111 serves as the PRD)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, Tokio, no new runtime deps)
- [x] Reuses existing config infrastructure (`plan_configuration_system.md`) and OCI client — no novel framework
**Domain Tags:** infrastructure, integration, security, api
**Supersedes:** Refines the *implementation surface and fallback model* anticipated by issue #122 (see "Decision Outcome").
**Superseded By:** N/A

---

## Context

OCX resolves a package identifier (`registry/repo:tag@digest`) and contacts that registry directly over HTTPS. In corporate networks the external registries OCX targets — `ocx.sh`, `ghcr.io`, `docker.io`, `quay.io`, `public.ecr.aws` — are **firewall-blocked**. The organisation instead runs an artifact manager (JFrog Artifactory, Sonatype Nexus, or Harbor) configured with **remote/proxy repositories** that cache those upstreams. OCX must transparently route registry traffic to that mirror **without changing the canonical identifier or the content-addressed digest**, so that an `ocx.lock` produced behind the mirror remains valid on a machine with direct egress, and vice versa.

Two distinct "mirror URL" stories exist (issue #122). Only the second is work:

1. **Server-issued redirect** (302/307 from a blob `GET`; normative in [OCI Distribution Spec v1.1](https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md)): Docker Hub → Cloudflare, GHCR → Azure Blob. **Already works transparently** via `reqwest`'s default redirect policy. Nothing to build.
2. **Client-declared mirror** (this ADR): client config maps an upstream registry to a replacement endpoint. **This is the work.**

### What already exists (do not rebuild)

The configuration subsystem (`crates/ocx_lib/src/config/`, shipped per `plan_configuration_system.md`) already provides:

- Tiered discovery + merge: system → user → `$OCX_HOME/config.toml` → `OCX_CONFIG` → `--config`; project-tier `ocx.toml`.
- `[registry] default` + `OCX_DEFAULT_REGISTRY` — **this already solves "change the default OCX registry for bare identifiers."** A bare `cmake:3.28` can be pointed at any host today.
- `[registries.<name>] url=` — named-registry lookup table (`RegistryConfig`, `crates/ocx_lib/src/config/registry.rs`), whose doc comment explicitly reserves *"location rewrite"* as a future field.

The gap this ADR closes: redirecting identifiers that **already name an explicit third-party host** (and, after default-injection, the default host too) to a corporate mirror that **prefixes the upstream path under a repo key** — and doing so as a **hard replacement** because the origin is unreachable.

### The single rewrite seam (discovered in code)

`From<&Identifier> for native::Reference` (`crates/ocx_lib/src/oci/identifier.rs:219`) is the one place a canonical registry becomes a network reference. It is invoked at ~15 call sites in `crates/ocx_lib/src/oci/client.rs`. Storage and identity are keyed off the **`Identifier`**, not the `native::Reference`: `blobs/{registry}/…`, `tags/{registry}/…`, `symlinks/{registry}/…`, and `ocx.lock` pins all use the canonical registry. Auth is resolved from the `native::Reference` passed to the transport (`NativeTransport::auth_for` keys off `image.registry()`, `crates/ocx_lib/src/oci/client/native_transport.rs:51`). Therefore a rewrite applied **only when building the `native::Reference`** redirects all transport (resolve, pull, catalog) and auth to the mirror while leaving every on-disk identity canonical.

### Requirements (confirmed with the requester)

| # | Requirement | Source |
|---|-------------|--------|
| R1 | Mirror URL is **host + repository-path prefix** (Artifactory repo-key path method — the Docker/OCI **pull** path `<host>/<repo-key>/owner/repo`, e.g. `company.jfrog.io/ghcr-remote/owner/repo`; *not* the `/artifactory/api/docker/<repo-key>` admin REST path). | Requester |
| R2 | **Replace semantics** — never contact the upstream origin when a mirror is configured; mirror failure is a hard error (egress is blocked, so fallback is dead weight and a security footgun). | Requester |
| R3 | **Per-host mappings** — each upstream host maps to its own mirror endpoint/repo-key. | Requester |
| R4 | Canonical identifier + digest unchanged; `ocx.lock` portable across mirrored / direct-egress hosts. | Product principle (content-addressed) |
| R5 | Mirror needs its own credentials (the proxy host, not the upstream). | Domain |
| R6 | Tamper resistance: a poisoned mirror must be detected and rejected. | Security |

---

## Decision Drivers

- **Blocked-egress correctness** — the feature is useless if any code path can still reach the open internet; "fail loud, never silently egress" is the governing safety property.
- **Content-address portability** — lockfiles and the object store must stay keyed by the canonical upstream host (Differentiator #2, Principle #2 offline-first).
- **Minimise submodule churn** — `external/rust-oci-client` is a patched fork; changes there need upstream PRs and are formatted to a foreign 100-col rustfmt (`subsystem-oci.md` gotcha). Prefer policy in `ocx_lib`.
- **Reuse the shipped config + auth + digest-verification machinery** — no new dependency, no new framework (KISS, Choose Boring Technology).
- **One-way-door cost** — the config key/shape becomes a public contract once a published `config.toml` uses it; get the schema right, keep the engine deferrable.

---

## Industry Context & Research

**Research artifact:** [`research_registry_mirror.md`](./research_registry_mirror.md)

**Trending approaches.** Surveyed containerd `hosts.toml`, Docker `registry-mirrors`, Cargo source replacement, Go `GOPROXY`, Maven `mirrorOf`, npm `.npmrc`, and the OCI proxy modes of Artifactory / Nexus / Harbor.

**Key insights driving the decision:**

1. **The repo-key path prefix is universal.** Artifactory, Nexus, and Harbor all insert their proxy-repo identifier as a path prefix between their host and the *verbatim* upstream path. A host-only rewrite (containerd's default) **cannot** address an Artifactory path-method repo. → host **and** path-prefix must be rewritable (R1).
2. **Cargo's "replacement must serve identical content" is the right integrity contract** and maps cleanly onto OCI content-addressing: verify the digest after every fetch and the mirror cannot tamper undetected (R6). Cargo also deliberately splits `[source] replace-with` (transparent mirror, same identity) from `[registries]` (different identity) — OCX should keep that semantic split.
3. **Origin fallback is a footgun for blocked egress.** Go's comma-list fallback exists for *availability*; in a firewall-controlled environment, falling through to the internet is the opposite of intent. Maven's "first match, no fallback" is the safe model (R2).
4. **containerd's per-upstream-host keying** is the cleanest selection model for "this upstream → that mirror" (R3); its `capabilities` concept informs read-vs-push scoping.
5. **mise/aqua have no OCI mirror support** — this is a genuine OCX differentiator for enterprise/air-gapped deployments.

---

## Considered Options

The decision has **two independent axes**: (A) the configuration semantics/selection model, and (B) where the rewrite is implemented.

### Axis A — Configuration semantics & selection model

#### A1: containerd ordered-fallback list (issue #122's original framing)

`[registries."ghcr.io".mirrors]` = ordered `Vec<MirrorEntry>` with capabilities; try each, **fall back to origin** on connection error / 5xx.

| Pros | Cons |
|------|------|
| Familiar to ops who know `hosts.toml` | Fall-back-to-origin contradicts R2 (blocked egress) — a footgun |
| Multi-mirror availability | Host-only; no path-prefix rewrite → cannot target Artifactory path method (fails R1) |
| Capability scoping built in | More config surface (lists, capabilities) than the requester needs now (YAGNI) |

#### A2: Cargo-style per-host source replacement + path-prefix rewrite (recommended)

`[mirrors."<upstream-host>"] url = "<scheme>://<host>[/<path-prefix>]"`. Exact-host match, **replace semantics, no origin fallback**, host + path-prefix rewrite, digest-verified.

| Pros | Cons |
|------|------|
| Directly satisfies R1–R6 | Single mirror per host (no multi-mirror availability) — acceptable; not required |
| "Fail loud, never egress" is the default — no footgun | New top-level `[mirrors]` section to document |
| Path-prefix rewrite handles Artifactory/Nexus/Harbor | Tag→digest resolution still trusts the mirror for unpinned tags (mitigated by digest verify + lockfile pinning) |
| Minimal schema; engine is a one-shot transform | |

#### A3: Maven `mirrorOf` wildcard model

`[[mirror]] of = "*" / "external:*,!ghcr.io" / "a,b"`, `url = …`.

| Pros | Cons |
|------|------|
| Richest selection (one Artifactory fronts everything) | Requester chose **per-host**, not wildcard (YAGNI now) |
| First-match-no-fallback matches R2 | Pattern engine (`*`, `!`, lists) is avoidable complexity for v1 |
| Natural superset of A2 | |

### Axis B — Where the rewrite lives

#### B1: single transform inside `oci::Client`, `Identifier` immutable (recommended)

`oci::Client` holds a `MirrorMap`. Reference construction for the **read path** is funnelled through **one** private method — `Client::transport_reference(&Identifier) -> native::Reference` (and a sibling `transport_registry(&str)` for the catalog case) — which applies the host + path-prefix rewrite. The `Identifier` is **never mutated** and **never carries mirror state**; the `MirrorMap` lives only on the `Client`. **Construction-gating** enforces coverage: the public `From<&Identifier> for native::Reference` impl is **removed** (a trait impl cannot be visibility-restricted, and `native::Reference` is an external type with no inherent-method seam), so `native::Reference::from(id)` no longer compiles anywhere; push uses a named `pub(crate) Identifier::canonical_reference()` and read uses `transport_reference`/`transport_registry` — a read-path site that bypasses the transform has no canonical-`From` symbol to call and **fails to compile**, not merely a test. A lightweight behavioural test backstops it. Recursion (image-index → child manifests, built via `clone_with_digest`) is covered automatically because every child fetch re-enters the same `Client` method.

| Pros | Cons |
|------|------|
| Zero submodule churn (no upstream PR, no foreign rustfmt) | OCX owns the rewrite logic (but it is OCX policy anyway) |
| **Canonical identity preserved *by construction*** — storage takes `Identifier`/registry-string only (verified, see Technical Details); mirror cannot reach storage | If multi-mirror fallback is ever needed, the retry loop lives in `Client` (acceptable; deferred) |
| Single transform point + structural guard → leak is a compile/test failure, not a latent bug | A `native::Reference` may surface in a *transport-layer* error message (mirror host) — acceptable/desirable; higher layers stay canonical |
| Auth + digest verification already correct at this layer | |
| Replace semantics need only a one-shot transform — no HTTP-layer retry | |

#### B2: `external/rust-oci-client` submodule `ClientConfig` (issue #123's stated surface)

Extend the fork's `ClientConfig` with a mirror map; `Client::pull_blob` consults it, retries, falls back at the connection layer.

| Pros | Cons |
|------|------|
| Connection-level fallback/redirect-allowlist natural here | Submodule = upstream-PR burden + foreign formatting (`subsystem-oci.md`) |
| Reuses upstream digest verification | Fallback retry is exactly what R2 removes — its main advantage is moot |
| | Path-prefix rewrite + capability scoping is OCX policy, awkward to push upstream |
| | Splits config policy (`ocx_lib`) from mechanism (fork) across a submodule boundary |

---

## Decision Outcome

**Chosen: Axis A → A2 (Cargo-style per-host source replacement with path-prefix rewrite, replace semantics, digest-verified), Axis B → B1 (rewrite at the `ocx_lib` client seam).** Schema is left open to grow into A3 (wildcard) without migration.

**Rationale.** The requester's constraints — blocked egress (R2), Artifactory repo-key prefix (R1), per-host (R3) — turn the problem from containerd's *availability* model (issue #122's original assumption) into Cargo's *source-replacement* model: a transparent, integrity-checked rewrite with no fallback. Once fallback is gone, the rewrite is a one-shot transformation with no connection-level retry, which makes the submodule's only real advantage (B2) moot and lets the feature live entirely in `ocx_lib` (B1) — preserving canonical identity by construction and avoiding fork churn. Digest verification (already implemented at `client.rs::verify_blob_digest`) is the Cargo "identical content" guarantee realised through OCI content-addressing, satisfying R6.

#### The `Identifier` stays pure-canonical (rejected: mirror-on-identifier)

A tempting alternative is to attach the mirror to the `Identifier` itself — either by rewriting `Identifier.registry()` to the mirror host, or by adding a transport-override field. **Both are rejected.**

- *Mutating `Identifier.registry()`* is unsafe: storage is computed exclusively from `identifier.registry()` (and `PinnedIdentifier`/registry-string derivations) — `blobs/`, `layers/`, `packages/`, `tags/`, `symlinks/`, and `ocx.lock`. A mirrored registry on the identifier would poison every one of them, breaking R4 and lockfile portability. This is the single most dangerous failure mode and the design must make it impossible.
- *Adding a transport-override field to `Identifier`* keeps storage correct but is strictly more risk than keeping the mirror out of the identifier: `Identifier` is a widely-cloned, widely-passed value, so an override field invites accidental leakage and forces every consumer to know which accessor is "the canonical one". It buys nothing — `Client` centralization already covers the recursive-fetch case the field would address.

**Therefore the mirror lives only on the `Client`, the transform is a single `Client` method, and `Identifier` remains an immutable canonical value.** The invariant — *every file-structure path is derived from the original identifier, never the mirrored reference* — is then guaranteed structurally (storage has no `native::Reference` input at all) rather than by convention.

> **Refinement of issues #122 / #123.** #122 anticipated a containerd-style ordered mirror list living in the submodule. This ADR records that the corporate requirement is **replace, not fallback**, and **path-prefix, not host-only**, so the model is source-replacement and the surface is `ocx_lib`, not the submodule. #123's acceptance criteria should be re-scoped accordingly (see Implementation Plan). The submodule remains the home for any *future* connection-level capability (multi-mirror fallback, redirect-target allowlist) if those are ever requested.

### Answers to issue #122's seven design questions

| # | Question | Decision |
|---|----------|----------|
| 1 | Config shape | New `[mirrors."<host>"]` table (host + optional path-prefix in `url`). Distinct from `[registries.<name>]` (identity) — mirrors are transparent transport, per Cargo's split. |
| 2 | Per-registry override | Keyed by upstream host; tier-merged like all config (higher tier replaces the matching host entry key-by-key). |
| 3 | Fallback order | **None.** Replace semantics; mirror failure → hard error (R2). Ordered-list + origin fallback deferred until a CDN/acceleration user appears. |
| 4 | Digest verification | Always, post-fetch (existing `verify_blob_digest`). Makes replacement safe; cannot be disabled in v1. |
| 5 | Capability scoping | Read path (`resolve` + `pull` + `catalog`) is rewritten. `push` is **not** mirror-redirected (proxy/remote repos are read-only); push uses the canonical registry. |
| 6 | Domain allowlist | Deferred. Replace semantics already bars upstream egress; server-issued redirects *from* the mirror to its own backing store must still be permitted, so an allowlist needs care. |
| 7 | Auth | Keys off the rewritten (mirror) host slug — `OCX_AUTH_<mirror_slug>_*` and `~/.docker/config.json` for the mirror host. Free at the B1 seam. |

### Consequences

**Positive:**
- Blocked-egress installs work; lockfiles stay portable (canonical identity preserved by construction).
- No `external/rust-oci-client` change for v1 → no upstream-PR dependency.
- Mirror auth reuses the existing `OCX_AUTH_*` + docker-cred path with zero new surface.
- Tamper-evident by default (digest verification on every fetch).
- Schema is forward-compatible toward wildcard (`[mirrors."*"]`) and optional fallback.

**Negative:**
- New public config contract (`[mirrors]`) — one-way door; must be documented before any published config relies on it.
- Single mirror per host in v1 (no multi-mirror availability).
- `ocx index catalog` / `_catalog` against a proxy lists only cached repos (a registry-side limitation, documented, not an OCX bug).

**Risks:**
- *Unpinned tag trust:* for a tag not pinned in `ocx.lock`, OCX trusts the mirror's tag→digest mapping (same trust as the origin). **Mitigation:** digest verification on the resolved manifest; recommend `ocx lock` so installs are digest-pinned, after which the mirror cannot substitute content undetected.
- *Push misroute:* silently redirecting `push` to a read-only proxy would fail confusingly. **Mitigation:** scope mirror rewrite to the read path; `push` targets the canonical host (design question #5).
- *Storage leak via a careless new call site:* the danger flagged in review — a future transport path that round-trips `Reference → Identifier → storage`, or that derives a path from the mirrored reference. **Mitigation (structural, not vigilance):** storage accepts no `native::Reference` input today (verified table above); keep it that way; funnel all read-path reference construction through the single `transport_reference`/`transport_registry` methods; enforce that via **construction-gating** — remove the public `From<&Identifier> for native::Reference` impl (so `native::Reference::from(id)` compiles nowhere) and give push a separate named `pub(crate) Identifier::canonical_reference()`, so any bypassing read-path reference has no canonical-`From` symbol to call and fails to compile (a behavioural test backstops it). A leak then fails the build, not production.

---

## Technical Details

### Architecture (C4 — Container/Component)

```
ocx.toml / config.toml  ──[mirrors."ghcr.io"]──┐
                                                 ▼
ConfigLoader ─► Config{ mirrors: HashMap<host, MirrorConfig> }
                                                 │  (resolution-affecting)
                                                 ▼
            Context::try_init ─► MirrorMap ─► oci::ClientBuilder.mirrors(map)
                                                 ▼
   PackageManager ─► oci::Client ─► transport_reference(&Identifier)
                                       │   canonical Identifier (ghcr.io/owner/tool@sha256:..)
                                       │       │ identity/storage/lock  ── UNCHANGED (ghcr.io)
                                       ▼       ▼
                              native::Reference{ registry=company.jfrog.io,
                                                 repo=ghcr-remote/owner/tool }
                                       ▼
                              NativeTransport ─► auth_for(company.jfrog.io) + HTTPS
                                       ▼
                              verify_blob_digest(@sha256:..)  ── tamper gate
```

### Storage isolation invariant (verified in code, 2026-06-02)

The correctness of the whole design rests on one invariant: **every file-structure path is derived from the canonical `Identifier`, never from the (possibly mirrored) `native::Reference`.** This was verified against the current tree, not assumed:

| Store | Registry segment source | Evidence |
|-------|------------------------|----------|
| `blobs/` | `pinned.registry()` / `blob_ref.registry()` / explicit `registry: &str` | `blob_store.rs:62,75,112`; callers `local_index.rs:209,312`, `pull_local.rs:377` |
| `layers/` | explicit canonical `registry: &str` | `layer_store.rs:62`; callers `pull.rs:803` (`pinned.registry()`), `pull_local.rs:265` |
| `packages/` | `identifier.registry()` / `fs.packages.path(pinned)` | `package_store.rs:154`; `pull.rs:425,649` |
| `tags/` | `identifier.registry()` | `tag_store.rs:40` |
| `symlinks/` | `identifier.registry()` | `symlink_store.rs:55` |
| `temp/` | `identifier.registry()` | `temp_store.rs:192,208` |

No store accepts a `native::Reference`. The reverse `Identifier::try_from(native::Reference)` appears on exactly **one** non-test, non-`PinnedIdentifier` path — `oci/client/error.rs:78`, which builds an *error message*, not a storage path. Manifest-chain blob digests come from the manifest content (content-addressed), so they are identical regardless of which host served the bytes.

**Consequence:** because storage has no `Reference` input, the mirror cannot reach storage even if a future call site is added carelessly — provided the one rule below holds. The structural guard test enforces it.

### Resolution flow (the one-shot transform)

1. CLI parses identifier; bare names get `[registry] default` injected → **canonical** `Identifier` (e.g. `ghcr.io/owner/tool:1.2`). *(Default-injection and mirror rewrite are independent and compose: a default-injected `ocx.sh/...` can itself be mirrored.)*
2. The canonical `Identifier` flows to `PackageManager` and `Client` unchanged. **Storage paths are computed here, from the canonical identifier.**
3. Only at the transport boundary, `Client::transport_reference(id)` builds the `native::Reference`: if `mirrors[id.registry()]` exists, `registry = mirror.host`, `repository = join(mirror.path_prefix, id.repository())`, tag/digest copied verbatim; else identical to today's `From<&Identifier>`. This reference is **local to the transport call and discarded** — it is never converted back into an `Identifier` for storage.
4. Transport contacts the mirror host; auth resolves for the mirror host; blob digest verified against the canonical digest.
5. Object store / tags / symlinks / `ocx.lock` write the **canonical** identifier (from step 2) — never the mirror host.

### Config schema (proposed public contract)

```toml
# Per-upstream-host replacement. The `url` is scheme + host + optional repo-key path prefix.
# This is the Docker/OCI PULL path (`<host>/<repo-key>`), not the `/artifactory/api/docker/...`
# admin REST path. OCX appends the upstream repository path verbatim after the prefix.
[mirrors."ghcr.io"]
url = "https://company.jfrog.io/ghcr-remote"

[mirrors."docker.io"]
url = "https://company.jfrog.io/dockerhub-remote"

# A bare default registry can also be mirrored:
[mirrors."ocx.sh"]
url = "https://company.jfrog.io/ocx-remote"
```

- Maps `ghcr.io/owner/tool:1.2` → host `company.jfrog.io`, repo `ghcr-remote/owner/tool`, tag `1.2`.
- A host-only mirror (subdomain method) is the degenerate case: `url = "https://ghcr-remote.company.jfrog.io"` (empty path prefix).
- Plain-HTTP mirrors compose with the existing `OCX_INSECURE_REGISTRIES` (the *mirror* host is listed, since that is what is contacted).
- Forward-compatible: a future `[mirrors."*"]` catch-all (A3) and an optional `fallback = true` (re-introduce origin fallback) drop into the same struct without breaking existing configs (root `Config` has no `deny_unknown_fields`; `MirrorConfig` does, so typos fail fast).

### Rust surface (design intent — not implementation)

```rust
// crates/ocx_lib/src/config/mirror.rs  (new; mirrors registry.rs conventions)
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MirrorConfig {
    /// scheme://host[/repo-key-path-prefix]; OCX appends the upstream repo path.
    pub url: Option<String>,
}

// Config root gains:  pub mirrors: Option<HashMap<String, MirrorConfig>>
// (key = canonical upstream host, e.g. "ghcr.io")

// crates/ocx_lib/src/oci/client.rs
impl Client {
    // The ONE place a mirror is applied. Read path only. Pure host+path-prefix
    // substitution; the input Identifier is the storage/identity authority and is
    // never mutated. The returned Reference is transport-only and is not converted
    // back into an Identifier for storage.
    fn transport_reference(&self, id: &Identifier) -> native::Reference { /* mirror or From<&Identifier> */ }
    fn transport_registry(&self, registry: &str) -> native::Reference { /* catalog op (list_repositories) */ }
}
```

Rules that make the invariant structural:

1. **`Identifier` gains no mirror field and is never mutated.** It stays a pure canonical value.
2. **Read-path reference construction goes only through `transport_reference` / `transport_registry`.** Enforced by **construction-gating**, not a source scan: the public `From<&Identifier> for native::Reference` impl is **removed** (`native::Reference::from(id)` compiles nowhere); push uses a named `pub(crate) Identifier::canonical_reference()`; read uses the two `Client` transform methods. A bypassing read-path reference has no canonical-`From` symbol and fails to compile. (Note: the launcher wire-ABI "canary" tests are *built-tree traversals*, not `include_str!` source scans — a source-text grep is the wrong model here; construction-gating is the correct one.) A behavioural `client.rs` test backstops the compile-time guard.
3. **Push path uses canonical references** (no mirror) — design question #5; the guard distinguishes read vs push sites.

The `url` is parsed/validated in the **config layer** (`config/mirror.rs`, at `Context::try_init` time) into a `ParsedMirror{ protocol, host, path_prefix }`; `oci::Client` receives an **already-parsed** `MirrorMap` and does **not** import `Config` (mirrors the `plain_http_registries(Vec<String>)` precedent). The split is a **dedicated** one — strip scheme → protocol, first `/` → host, remainder verbatim → path-prefix, trim one trailing `/` — and must **not** reuse `auth::registry_url::canonicalize_registry`, which strips a trailing `/vN` and special-cases `docker.io`, corrupting a repo-key prefix or a `docker.io` mirror host. `OcxConfigView` gains the mirror map so it is **forwarded to child `ocx` subprocesses** via `Env::apply_ocx_config` (resolution-affecting — `subsystem-cli.md` "OCX Configuration Forwarding"); the launcher/`run`/`exec` chain must see the same mirrors. The forwarding channel is a **single JSON-encoded** `OCX_MIRRORS` env var (structured URLs make the `OCX_INSECURE_REGISTRIES` comma/`=` scheme fragile; JSON has no delimiter ambiguity and the child is the same pinned binary, so no version envelope is needed), documented in `website/src/docs/reference/environment.md`.

### Interactions

| Concern | Behaviour |
|---------|-----------|
| `[registry] default` / `OCX_DEFAULT_REGISTRY` | Independent, runs first (parse time). Mirror runs at transport. They compose. |
| `--offline` | No transport at all → mirrors irrelevant (local index/digest only). |
| `--remote` | Mutable lookups hit the **mirror** (rewritten), not the origin. |
| `ocx.lock` | Stores canonical host + digest; unaffected by mirror; portable. |
| `push` | Not mirror-redirected (read-only proxy); uses canonical host. |
| Auth | Mirror host credentials (`OCX_AUTH_<mirror_slug>_*`, docker config). |

---

## Implementation Plan (re-scopes #123)

1. [ ] **Config** — add `MirrorConfig` + `Config.mirrors`; merge + `deny_unknown_fields`; unit tests (round-trip, tier merge, url→(host,prefix) split). *(`crates/ocx_lib/src/config/mirror.rs`, `config.rs`)*
2. [ ] **Client transform (single point)** — `MirrorMap` on `Client` (built in `ClientBuilder`); add `transport_reference`/`transport_registry`; route the read-path reference sites through them; leave push sites canonical; **`Identifier` unchanged**. Construction-gating: remove the public `From<&Identifier> for native::Reference` impl + add named `pub(crate) Identifier::canonical_reference()` for push (a bypassing read reference has no `From` symbol → fails to compile); behavioural backstop test; storage takes no `Reference`. *(`oci/client.rs`, `oci/client/builder.rs`, `oci/identifier.rs`)*
3. [ ] **Auth + insecure** — verify auth keys off mirror host; mirror host composes with `OCX_INSECURE_REGISTRIES`.
4. [ ] **Forwarding** — `OcxConfigView` + `Env::apply_ocx_config` + env-var doc for subprocess parity.
5. [ ] **Acceptance test** — mock upstream + mock mirror (path-prefix); assert: pull/resolve routed to mirror, push not redirected, poisoned-mirror digest mismatch rejected, no `mirrors` config = identical behaviour to today, lockfile records canonical host. *(`test/tests/`)*
6. [ ] **Docs** — new "Registry mirrors" section in Configuration reference; env-var reference; user-guide corporate/air-gapped note. Mention the existing `[registry] default` for the bare-default case.
7. [ ] **Schema** — `ocx_schema` regen so `config.toml` `[mirrors]` validates.

Defer (open, not in v1): wildcard `[mirrors."*"]` + exclusions (A3); optional `fallback = true` ordered list (containerd model, submodule B2); redirect-target domain allowlist (#122 Q6).

## Validation

- [ ] Acceptance test: mirror routing + path-prefix + digest verification + push-not-redirected + no-regression (step 5).
- [ ] Security review (`/security-auditor`): blocked-egress guarantee (no path reaches origin when a mirror is set), unpinned-tag trust model, mirror-auth handling.
- [ ] `task verify` green; `ocx_schema` validates `[mirrors]`.
- [ ] Docs reviewed against `docs-style.md`; env var added to `environment.md` and forwarded in `apply_ocx_config`.
