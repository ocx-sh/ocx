# Research: Transparent Tag Fallback Patterns

**Date:** 2026-04-12
**Related Issue:** [ocx-sh/ocx#41](https://github.com/ocx-sh/ocx/issues/41)

## Industry Patterns

| Tool | Strategy | Key Detail |
|------|----------|------------|
| **Cargo (sparse)** | Local-first with conditional HTTP revalidation | Fetches only the specific crate's index entry via HTTP `ETag`/`If-None-Match`. Single-entry targeted fetch. |
| **Go modules** | Cascading proxy list | `GOPROXY=https://proxy.golang.org,direct` — tries each in order. 404 = terminal, network error = next proxy. |
| **npm/pnpm** | Always-remote for metadata | No local-first for package metadata. Differentiates 404 (terminal) from 5xx (retry). |
| **Docker/containerd** | Mirror chain fallback | Static ordered mirror list; fallback to upstream on non-200. Pull-through caches placed first. |
| **mise** | TTL-based full re-fetch | Entire version list cached with 24h TTL. Less targeted than needed for OCX. |

**Core insight from Cargo and Go:** Both perform **targeted single-entry fetch** (one crate file, one module version) rather than re-syncing the whole index. Both distinguish "not found on remote (404)" from "network failure (timeout/5xx)" — the former terminates, the latter retries or cascades.

## Pattern Classification for OCX

| Pattern | Verdict |
|---------|---------|
| Always remote first | Reject — breaks offline-first principle |
| Explicit sync required (current) | Poor UX — hostile for interactive use |
| **Local first, remote fallback, persist** | **Recommended** — matches Cargo sparse, Go modules |
| Remote fallback without persist | Wrong — re-fetches every resolve |

## Implementation Pattern Comparison

| Pattern | Changes Required | Pros | Cons |
|---------|-----------------|------|------|
| **Composite ChainedIndex** | New IndexImpl, context wiring | Zero changes to PackageManager or callers; co-locates fallback with index layer | New type + refactoring update_tag to &self |
| Retry-at-caller | resolve(), resolve_all(), find_or_install(), etc. | Most explicit | 6+ call sites change; every new caller needs same logic |
| Strategy in PackageManager | PM constructor, resolve() | Fine-grained per-task control | PM gains complexity; needs mutable local_index access |

**Recommendation: ChainedIndex (cache + ordered source chain).** OCX already has the exact primitives needed (`LocalIndex::update_tag` does targeted single-tag fetch and persist). The ChainedIndex wraps a `LocalIndex` cache and a `Vec<Index>` of read-only sources under the existing `Box<dyn IndexImpl>` trait — zero API surface change above the index layer. The Vec shape is forward-compatible with future N-source scenarios (CI pre-warmed cache, corporate mirror) without an API rename.

## OCI-Specific Findings

- **Single-tag resolution:** `HEAD /v2/<name>/manifests/<tag>` returns `Docker-Content-Digest` header in one roundtrip. All major registries (Docker Hub, GHCR, ECR, Harbor) support this per OCI Distribution Spec.
- **Error mapping concern:** `NativeTransport::fetch_manifest_digest` maps all errors via `registry_error` (not `manifest_not_found_or_registry_error`). A 404 on HEAD becomes `Err(ClientError::Registry(...))` not `Ok(None)`. The ChainedIndex must catch errors from `update_tag` and degrade gracefully.
- **Rate limits:** Docker Hub 40 pulls/hr (auth) includes HEAD requests. Per-tag HEAD is strictly cheaper than full `list_tags`. Negative caching (tracking tags confirmed absent) is a future optimization.
- **Concurrency:** Parallel fallback requests for different tags are safe. `LocalIndex::update_tag` merges into the on-disk tag file idempotently — concurrent writes for the same tag produce the same result.

## Sources

- [Cargo sparse index RFC 2789](https://rust-lang.github.io/rfcs/2789-sparse-index.html)
- [Go Modules Reference — GOPROXY](https://go.dev/ref/mod)
- [OCI Distribution Spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md)
- [Docker Hub Auth/Rate Limits](https://toddysm.com/2024/01/30/authenticating-with-oci-registries-docker-hub-implementation/)
