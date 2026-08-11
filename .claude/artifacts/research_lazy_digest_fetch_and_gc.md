# Research: Digest-Pinned Deferred Fetch, Non-Interactive Auth, and GC of Derived Artifacts

- **Date:** 2026-08-09
- **Axis:** domain (OCI distribution semantics + GC models)
- **Consumed by:** [`adr_lazy_package_loading.md`](./adr_lazy_package_loading.md), [ocx-sh/ocx#302](https://github.com/ocx-sh/ocx/issues/302)
- **Companions:** [`research_lazy_shim_prior_art.md`](./research_lazy_shim_prior_art.md), [`research_lazy_shim_robustness.md`](./research_lazy_shim_robustness.md)

## 1. OCI digest semantics

A digest GET (`/v2/<name>/manifests/sha256:…`, `/blobs/…`) is content-addressed and
immutable per spec — same digest, byte-identical content, safe to cache forever with no
revalidation. It is **not** exempt from auth or rate limiting:

- Every registry still requires `Bearer` auth scoped `repository:<name>:pull` for the
  digest GET. Confirmed locally: `Client::pull_blob` calls
  `ensure_auth(&image, RegistryOperation::Pull)` even for a pure digest-addressed blob
  fetch (`crates/ocx_lib/src/oci/client.rs:652-655`).
- **Docker Hub counts every manifest GET toward the pull quota, tag or digest.** No
  surveyed registry (Docker Hub, GHCR, ECR) discounts digest GETs.

**Consequence for lazy loading:** deferral moves registry cost in time; it does not
reduce it. A fleet that defers and then triggers still spends the same quota, and
spends it at build time rather than at setup time.

## 2. `--frozen` vs `--offline`, verified against the code

`ChainMode` (`.claude/rules/subsystem-oci.md`) already draws the exact line the lazy
design needs:

| Mode | Digest-addressed miss |
|---|---|
| `Frozen` | **walks the source** — only *unpinned-tag resolution* is policy-blocked (exit 81) |
| `Offline` | returns `None` — no network, ever |

So a lazy shim **can** materialize by digest alone under `--frozen`, and **must** refuse
under `--offline`. `PinnedIdentifier`
(`crates/ocx_lib/src/oci/pinned_identifier.rs:20-27`) is the "digest guaranteed present"
type this path consumes — the same type `resolve.json` already stores so "consumers never
need fallback resolution logic".

This is a verification of an existing routing matrix, not a new policy.

## 3. Non-interactive auth from a spawned child

Every mature credential system converges on the same shape: a short-lived subprocess
speaks one JSON request/response over stdio, is **non-interactive by contract**, and on
missing credentials exits cleanly rather than prompting. Precedent: git credential
helpers (silent exit, `GIT_TERMINAL_PROMPT=0`), Cargo's credential-provider protocol,
the EngFlow/Bazel credential-helper spec. None falls back to a TTY prompt from a child —
that stays with the outer interactive command (`docker login`, `cargo login`).

**Local state:** `Auth::get_impl` (`crates/ocx_lib/src/auth.rs:39-48`) tries
`OCX_AUTH_<registry>_{TYPE,USER,TOKEN}`, then `get_docker_auth` (`auth.rs:89-118`, via
the `docker_credential` crate), then falls back to `Anonymous`. `get_or_fallback`
(`auth.rs:51-64`) swallows **errors** from that chain, not just "not found", into
anonymous with a warning.

No blocking prompt is reachable from a spawned child — correct by construction. But the
error-swallowing means a misconfigured or expired credential helper degrades to an
anonymous attempt and surfaces as a **401 from the registry**, not as an auth error. From
inside a build tool that is a confusing failure. Whether that degrade is right for the
shim path is a decision the ADR should make explicitly rather than inherit.

## 4. GC of derived, regenerable artifacts — link count is rejected everywhere

| System | Liveness signal | Notes |
|---|---|---|
| **Nix** | explicit GC roots (symlinks under `/nix/var/nix/gcroots`), walked as a reachability graph | hardlink dedup (`nix-store --optimise`) is a **separate, storage-only** feature, orthogonal to GC |
| **Cargo** | last-use **timestamps in a SQLite DB** (stabilized auto-GC) | not inode `nlink` |
| **Bazel** | disk-cache LRU by size/age (`--experimental_disk_cache_gc_max_size/-age`, 7.4+) | — |
| **OCX (today)** | explicit `refs/{symlinks,deps,layers,blobs}` + single BFS reachability pass | already the industry-converged shape |

**Why `nlink` is rejected**: it cannot attribute a shared blob to a specific logical
owner. A CAS entry shared by two unrelated roots still looks referenced after one owner
is gone, and nlink says nothing about *which* roots hold it — exactly what selective GC
needs.

**Consequence for lazy loading:** any liveness question for shim artifacts extends the
existing `refs/*` reachability model. For the shared shim executable specifically, the
cheapest correct answer is **no collection at all** — the set is bounded by
(ocx version × arch), each entry is a few hundred KB, and it is regenerable from the
`include_bytes!`-embedded blob, so a GC surface buys nothing.

## Trends

- **Established** — JSON-over-stdio credential subprocesses; content-hash addressing as
  the cache/trust unit; explicit-root reachability GC over inode heuristics.
- **Emerging** — `uv auth` (Astral) applying the same non-interactive-first,
  explicit-credential-source model outside the Rust/Bazel world.
- **Declining** — registry-specific rate-limit carve-outs for digest pulls; none of the
  three major registries give one.

## Sources

- [OCI distribution-spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) — digest immutability, GET shapes
- [Docker token auth spec](https://distribution.github.io/distribution/spec/auth/token/) — `repository:<name>:pull` scope
- [Docker Hub rate limit, HEAD requests (Dec 2024)](https://www.augmentedmind.de/2024/12/15/docker-hub-rate-limit-head-request/) — every manifest GET counts
- [GHCR guide (Apr 2026)](https://www.gecko.security/blog/ghcr-github-container-registry-guide)
- [AWS ECR quota announcement](https://aws.amazon.com/about-aws/whats-new/2020/02/ecr-raises-simplifies-image-api-quotas-start-new-workloads-quicker) — 2020, no later revision found; **flag for re-verification**
- [git credential helpers](https://git-scm.com/docs/gitcredentials)
- [Cargo credential-provider protocol](https://doc.rust-lang.org/cargo/reference/credential-provider-protocol.html)
- [EngFlow credential-helper-spec](https://github.com/EngFlow/credential-helper-spec/blob/main/spec.md)
- [Nix GC roots](https://nix.dev/manual/nix/2.34/package-management/garbage-collector-roots)
- [NixOS storage optimization](https://wiki.nixos.org/wiki/Storage_optimization) — hardlink dedup ≠ GC
- [Bazel disk cache GC](https://github.com/bazelbuild/bazel/issues/5139)
- [Cargo cache cleaning](https://hackmd.io/@rust-cargo-team/HywNkwYHp) — SQLite last-use tracking
- [uv authentication](https://docs.astral.sh/uv/concepts/authentication/cli/)

## Recommendation

1. Digest-pinned fetches are cache-forever but cost one authenticated request and one
   rate-limit unit per miss. Do not design as if digest addressing were free.
2. Reuse `--frozen` / `--offline` as-is; the routing matrix already draws the correct
   line. Do not invent a lazy-specific network flag.
3. Decide explicitly whether the shim path keeps `get_or_fallback`'s degrade-to-anonymous
   behavior. Inside a build, a 401 is a worse signal than an auth error.
4. Never reach for `nlink`. Extend `refs/*`, or — for a bounded, regenerable, shared
   artifact — collect nothing.
