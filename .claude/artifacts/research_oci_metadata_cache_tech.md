# Research: OCI Metadata Caching — Technology/Tools Axis

> Axis: technology/tools. Produced by worker-researcher (sonnet) for the inspect-binaries-closure plan, 2026-07-20. Persisted by orchestrator (researcher session has no Write tool).

## Direct answer

Every tool surveyed (ORAS, crane, regclient, containerd, Docker) converges on the same two rules, and none do TTL/ETag-based manifest caching:

- **Digest-addressed content → cache forever** (immutable, content-addressed, no invalidation needed).
- **Tag-addressed resolution → always re-checked online**, best case via a cheap `HEAD` (Docker-Content-Digest header), never a cached/TTL'd answer.

This is exactly OCX's existing invariant (`subsystem-oci.md`: "cache never invalidated… call update() for fresh data") combined with digest-first addressing — the converged, boring-tech answer, not a gap.

## Per-tool detail

- **ORAS** (`ORAS_CACHE`, [oras-go content-oci](https://pkg.go.dev/oras.land/oras-go/v2/content/oci)): local dir as content-addressable OCI-layout store (`blobs/` + `index.json`), digest-keyed. `oras-go` v2 `content.FetchCache` ([Targets.md](https://github.com/oras-project/oras-go/blob/main/docs/Targets.md)) memory-caches manifests during multi-platform graph walks; warns never to cache large leaf blobs.
- **crane** ([crane.md](https://github.com/google/go-containerregistry/blob/main/cmd/crane/doc/crane.md)): deliberately stateless — "no local image cache" is a selling point. `crane pull -c/--cache-path` is a one-off per-invocation layer cache.
- **regclient** ([regclient.org/usage](https://regclient.org/usage/)): `ocidir://` treats a local dir as a full read/write OCI-layout target. GC on every write via reachability from `index.json` — not TTL.
- **containerd** ([content-flow.md](https://github.com/containerd/containerd/blob/main/docs/content-flow.md), [issue #3880](https://github.com/containerd/containerd/issues/3880)): splits "resolve" (tag→digest, always network) from "fetch" (digest→bytes, cacheable) — OCX's `IndexOperation::{Query,Resolve}` / `ChainMode` split, but type-enforced in OCX vs caller discipline in containerd (whose issue asking for this remains open since ~2020).
- **Docker/moby** ([moby#47875](https://github.com/moby/moby/issues/47875)): manifest cache kept until HEAD returns a different digest; no ETag/conditional-GET convergence industry-wide.

## Rust ecosystem

- **`oci-client`** (OCX vendored fork `external/rust-oci-client`): deliberately thin HTTP transport, no local store. `fetch_manifest_digest` already does HEAD-with-`Docker-Content-Digest`-then-GET-fallback, matching Docker/containerd.
- **`ocidir`** ([lib.rs/crates/ocidir](https://lib.rs/crates/ocidir)): v0.8.0 (Jul 2026), ~15k dl/mo, cap-std-sandboxed OCI Image Layout. **Not recommended**: OCX's blob path (`blobs/{registry}/{algo}/{2hex}/{rest}/`) is registry-namespaced across repos; single-repo OCI Image Layout doesn't model that — adoption would be a downgrade.
- **`oci-spec`**: spec types; OCX deliberately avoids it (`adr_custom_oci_identifier.md`). No change indicated.

## Trend scouting

- **OCI 1.1 referrers API** (`GET /v2/<name>/referrers/<digest>`, [distribution-spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md), [OCI 1.1 announcement](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/)): the one standardized metadata-only endpoint (SBOM/signature/attestation discovery). Broad support (ECR, GHCR, Cloudsmith, zot). Standalone capability gap for OCX — a *feature*, not a caching-architecture change; own future ticket.
- [nesbitt.io — What Package Registries Could Borrow from OCI](https://nesbitt.io/2026/02/18/what-package-registries-could-borrow-from-oci.html) (2026-02): keep digest-addressed integrity, hide raw manifest mechanics behind metadata APIs — validates OCX's facade boundary. No action.

## Recommendation

OCX's `LocalIndex` + `ChainedIndex` + `ChainMode` + `IndexOperation` design is a **superset** of every surveyed tool — ORAS's local CAS + containerd's resolve/fetch split + Docker's digest-cache-forever rule, compile-time-enforced. **No architecture change indicated.** Don't adopt `ocidir`/`oci-spec`. Build the metadata-only closure walker on the existing `Index` facade. Separately-scoped future opportunity: OCI 1.1 referrers consumption.
