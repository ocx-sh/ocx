---
title: Blob / Cache Retention Policies for a Content-Addressed Binary Store
date: 2026-04-13
domain: storage | gc | infrastructure
triggered_by: Issue #35 — originally framed as "policy-based retention". Scope was later split — #35 became a pure-reachability fix (see `plan_resolution_chain_refs.md`). This research applies to the deferred follow-up for policy-based retention (shared-$OCX_HOME CI scenarios, LRU/TTL/size-cap for already-unreferenced blobs).
expires: 2027-04-13
status: prior-art for deferred follow-up issue
---

# Research: Blob/Cache Retention Policies for a Content-Addressed Binary Store

## Direct Answer

OCX should adopt **hybrid max-age + size-cap + explicit pinning** tracked via a sidecar SQLite index (not filesystem atime). The two TTL-eligible blob categories (resolution-cache manifests and prefetched tarballs) share a size-cap budget and each have a distinct default TTL. Eviction runs at end-of-command, at most once per day, skipped in `--offline` mode. In-flight downloads are already protected by OCX's existing `TempStore` staging directory; the sidecar index must be written only after the `temp/ → blobs/` rename completes.

## Prior Art Survey

### Cargo — SQLite last-use tracking with max-age + size-cap (most directly relevant)

Since Rust 1.75, Cargo maintains `~/.cargo/.global-cache` (SQLite) with six tables tracking last-access timestamps for all cache tiers. `DeferredGlobalLastUse` batches timestamp updates in memory and flushes them at command end. A `UPDATE_RESOLUTION` constant (300 seconds) prevents writes more frequent than once per 5 minutes per entry, keeping write overhead negligible. Eviction runs at most once per day (gated on `global_data.last_auto_gc`), skipped in `--offline`/`--frozen` mode. Age-based deletion uses `DELETE WHERE timestamp < threshold`. Size-based deletion orders by timestamp ascending and deletes oldest until under budget. The two modes compose: age runs first, then size-cap trims remaining entries. The design explicitly avoids atime because Docker volume mounts and network filesystems cannot be relied upon to update it.

Default TTLs: rebuilt-from-source artifacts at 1 month, downloaded blobs at 3 months.

Sources: [Cargo Cache Cleaning](https://blog.rust-lang.org/2023/12/11/cargo-cache-cleaning/), [global_cache_tracker.rs](https://github.com/rust-lang/cargo/blob/master/src/cargo/core/global_cache_tracker.rs).

### containerd — lease TTLs with reachability GC, no size-based eviction

Containerd uses label-based mark-sweep. Leases protect blobs via `containerd.io/gc.ref.content.*` labels. Default lease TTL is 24 hours — if the client process dies, the lease expires automatically. The `containerd.io/gc.expire` label accepts an RFC3339 timestamp; the background GC goroutine deletes the lease after expiry.

**Critical finding:** containerd has *no* size-based eviction. GitHub issue #6583 has requested it for years; the maintainer response is consistently "use Kubernetes node-pressure eviction at a higher layer." This is not viable for a developer tool. The absence of size-based eviction in a production-grade CAS at scale confirms the design space is genuinely hard — and that OCX must ship it rather than defer it.

Sources: [garbage-collection.md](https://github.com/containerd/containerd/blob/main/docs/garbage-collection.md), [issue #6583](https://github.com/containerd/containerd/issues/6583).

### Proxmox Backup Server — atime-based two-phase GC with explicit grace period

PBS uses `utimes()` to update chunk atime during the mark phase, then deletes chunks whose atime predates a cutoff in the sweep phase. The cutoff is `min(oldest active backup writer start time, now - 24h5m)`. The 24-hour window is both the in-flight grace period and the atime reliability hedge — PBS explicitly calls `utimes()` itself rather than relying on kernel read-triggered atime updates.

Cautionary tale: PBS makes atime work only because it controls its own `utimes()` call during mark. OCX cannot assume control over filesystem mount options (Docker mounts, CI runners, network mounts all commonly disable or throttle atime).

Source: [PBS Maintenance Docs](https://pbs.proxmox.com/docs/maintenance.html).

### Nix — symlink-root reachability GC, no TTL or size cap

GC starts from `/nix/var/nix/gcroots/` and marks all reachable paths. No TTL. No size budget. Users must manually delete old profile generations. This is the right model for the **package-referenced** blob category (already implemented in OCX via `refs/symlinks/` BFS) but offers nothing for orphaned non-package blobs.

Source: [Nix Pills — GC](https://nixos.org/guides/nix-pills/11-garbage-collector.html).

### Bazel disk cache — LRU + size cap, eviction delegated to bazel-remote

Bazel's native local disk cache grows unboundedly. Issue #5139 (opened 2018, open in 2026) tracks automatic GC. `bazel-remote` fills the gap with `--max_size` (LRU size cap) and `--cache_max_age` (TTL). Lesson: do not defer size-based eviction — it is the single most common user complaint in the Bazel ecosystem.

Sources: [Bazel issue #5139](https://github.com/bazelbuild/bazel/issues/5139), [bazel-remote docs](https://the-pi-guy.com/blog/managing_bazels_build_cache_and_eviction_policies/).

### Go module cache — nuclear option only

`go clean -modcache` deletes the entire cache. No TTL, no size cap, no selective eviction. Do not emulate.

Source: [Go Modules Reference](https://go.dev/ref/mod).

### OCI distribution spec — reachability-only, no TTL standard

OCI dist-spec v1.1 added the referrers API: untagged artifacts with a live `subject` reference are protected from GC. No TTL or size-based eviction is standardized. Registry-side GC remains implementation-specific. The referrers model applies to registry-side cleanup, not local store cleanup.

Source: [OCI Specs v1.1 Release](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/).

## Industry Context and Trends

**Established patterns.** SQLite sidecar for last-use tracking (Cargo since 1.75), hybrid TTL + size-cap composition (Cargo, bazel-remote), staging directory for in-flight protection (Cargo `.crate.part`, OCX `TempStore`), symlink-root reachability for protected blobs (Nix, Git, OCX already).

**Trending.** SQLite is the dominant choice for local tool metadata stores in 2024-2026 (Cargo, uv, mise, proto all use it). Explicit pinning APIs are gaining adoption in CI toolchain managers (mise `pin`, proto `pin`).

**Declining.** atime-based GC (unreliable on modern Linux, Docker, NFS). Background GC daemons for single-user CLI tools (coordination complexity without benefit). All-or-nothing cache deletion (Go's approach is widely criticized).

## Design Patterns for Retention in CAS

### LRU vs TTL vs size-cap vs hybrid

Pure LRU requires a complete ordering maintained in the index — correct but the index is the only practical place to store it. Pure TTL evicts useful items that simply haven't been accessed recently. Pure size-cap is fair under pressure but may evict blobs that are still very useful. The winning pattern across every surveyed tool is **hybrid**: TTL removes obviously stale items cheaply first, then size-cap trims remaining entries by age if the budget is exceeded. This matches Cargo's `max-download-age` + `max-download-size` composition exactly.

### Tracking last-access cheaply

| Mechanism | Reliability | Cost | Cross-platform |
|---|---|---|---|
| Kernel atime | Low — `noatime`, `relatime`, Docker mounts | Zero | Poor |
| Explicit `utimes()` on read | Medium — OCX controls the call | One syscall per read | Good |
| SQLite sidecar with coalesced writes | High | Negligible (300 s coalesce, end-of-command flush) | Excellent |
| Per-blob JSON sidecar file | Medium — not atomic for concurrent writers | One file write per read | Good |

SQLite with write coalescing is the right choice. It is the only mechanism that survives Docker volume mounts, NFS, and CI runner environments without relying on kernel behaviour.

### Pinning mechanisms

Three patterns observed: symlink roots (Nix/Git — already used in OCX for package-referenced blobs), lease tokens (containerd — suited for daemons, overkill for OCX), and explicit pin records in the index (simplest, sufficient for a single-process tool). OCX needs explicit pins only for the prefetched/portable category where CI workflows need to guarantee a blob survives across runs.

### Eviction triggers

On-write triggers (check quota before each write) add latency to the write path. Background daemons add lifecycle complexity. The right balance for a CLI tool is **end-of-command once per day** (Cargo's model): check `gc_state.last_auto_gc` at command completion, run eviction if more than 24 hours have elapsed, skip in `--offline` mode. An explicit `ocx clean [--max-age] [--max-size] [--dry-run]` command gives users direct control and CI pipelines a predictable hook.

### In-flight race protection

The classic hazard: GC runs while a download is in progress; the blob is written to `blobs/` after GC's scan but before GC completes — except GC deletes by reading from the sidecar index, and the index is only updated after `rename(temp/ → blobs/)`. The staging directory is therefore the primary defence: blobs live in `TempStore` (not registered in the sidecar index) until complete.

Two additional guards are warranted:

1. Register a sidecar index row only **after** `fs::rename` returns `Ok`.
2. GC skips any index entry with `created_at > now - 5 min` as a clock-skew and crash grace window.

OCX's existing `TempStore` already implements the staging half. The sidecar index just needs to honour the "write after rename" invariant.

## Known Pitfalls

**atime on modern Linux.** `relatime` is the default mount option since kernel 2.6.30. With `relatime`, atime is only updated when the current atime predates the mtime, or more than 24 hours have passed since the last atime update. Docker volume mounts typically disable atime entirely. Never use kernel atime for GC decisions in a tool that runs in CI.

**Clock skew and TTL.** If `OCX_HOME` is on a network filesystem, `SystemTime::now()` on the client can skew ±minutes from the server's mtime. Mitigation: use day-granularity TTLs and tolerate ±1-day errors as acceptable. Do not use sub-hour TTLs.

**Eviction stampede.** A bulk `index update` that caches thousands of resolution manifests at once will age-out simultaneously after the TTL. Mitigation: cap deletions per GC run (e.g., 500 blobs) and carry overflow to the next daily run. Log the count so users can understand disk pressure.

**Correctness hazard — mixing reachable and unreachable blobs.** OCX's three-tier CAS cleanly separates package-referenced blobs (tracked via `refs/blobs/`, GC'd by reachability BFS) from resolution-cache and prefetched blobs. The sidecar index must track **only TTL-eligible entries** — never blobs that are reachable from a package. Simplest enforcement: any blob written as part of a package install (pull pipeline) is never inserted into the sidecar index. Only `index update` (resolution cache) and `ocx prefetch` / bundle operations are sidecar-eligible writes.

**Surprising finding.** Containerd still has no size-based eviction after years of requests. This is the strongest validation that OCX must implement size-based eviction rather than treating it as a future item. If containerd at CNCF scale has not shipped it, the problem is underestimated in complexity. Start with the Cargo approach (size-ordered deletion by age), which is proven and simple.

## Recommendation for OCX

### Policy shape: hybrid TTL + size-cap + explicit pins

Three retention classes:

| Blob category | Default max-age | Size-cap participation | Pin mechanism |
|---|---|---|---|
| Package-referenced (live `refs/blobs/` edge) | Never — reachability GC only | No | Reachability via `refs/blobs/` forward-ref |
| Resolution cache (manifests from `index update`) | 90 days | Yes | None by default |
| Prefetched / portable (tarballs for offline bundles) | 365 days | Yes | Explicit `ocx pin add` |

Resolution-cache and prefetched blobs share a combined size-cap budget. Proposed default: **5 GiB**. When the budget is exceeded, both pools are drained together ordered by `last_used ASC` until total drops below the budget, regardless of whether individual entries have exceeded their TTL.

### Tracking mechanism: sidecar SQLite at `$OCX_HOME/blobs/.cache-index.db`

Proposed schema:

```sql
CREATE TABLE blob_cache (
    digest       TEXT    PRIMARY KEY,  -- "sha256:{hex}"
    registry     TEXT    NOT NULL,     -- slugified registry hostname
    category     TEXT    NOT NULL,     -- "resolution" | "prefetched"
    size_bytes   INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,     -- Unix seconds
    last_used    INTEGER NOT NULL,     -- Unix seconds
    pinned       INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE pin (
    digest     TEXT PRIMARY KEY REFERENCES blob_cache(digest),
    label      TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE gc_state (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL               -- e.g. "last_auto_gc" -> ISO 8601
);
```

**Write coalescing.** Buffer `last_used` updates in memory during command execution, flush once at command end. Skip flush in `--offline` mode. Use a 300-second resolution threshold (same as Cargo) to avoid write amplification when the same blob is accessed multiple times in one session.

### Eviction trigger: end-of-command, daily frequency

At the end of any command that writes to `blobs/`, check `gc_state WHERE key = 'last_auto_gc'`. If more than 24 hours have elapsed, run:

1. **Age-based deletion:** `DELETE FROM blob_cache WHERE last_used < (now - max_age_seconds) AND pinned = 0`
2. **Size-based deletion** if budget exceeded: SELECT all non-pinned entries `ORDER BY last_used ASC`, accumulate `size_bytes`, mark oldest for deletion until total fits in budget.
3. **Cap per-run deletions at 500 entries** to avoid stampede.
4. **Remove actual blob directories** from `BlobStore` for each deleted index entry.
5. Update `last_auto_gc`.

Exposed as `ocx clean [--max-age <duration>] [--max-size <bytes>] [--dry-run]`.

### In-flight protection

`TempStore` already provides staging. The sidecar index must honour one invariant: **register a blob entry only after `fs::rename(temp_dir, blob_dir)` returns `Ok`**. Additionally, skip index entries with `created_at > now - 5 min` during GC as a belt-and-suspenders guard.

### Pin mechanism

`ocx pin add <digest> [--label <label>]` inserts into `pin` and sets `pinned = 1`. `ocx pin list` and `ocx pin remove` round out the API. Pinned entries are excluded from both TTL and size-cap eviction. Minimal interface CI pipelines need to guarantee a bundle blob survives across runs.

## Sources

- [Cargo Cache Cleaning](https://blog.rust-lang.org/2023/12/11/cargo-cache-cleaning/) — auto-GC trigger design, TTL defaults, SQLite rationale, Docker atime problems
- [global_cache_tracker.rs](https://github.com/rust-lang/cargo/blob/master/src/cargo/core/global_cache_tracker.rs) — SQLite schema, `UPDATE_RESOLUTION` constant, age + size eviction queries
- [containerd garbage-collection.md](https://github.com/containerd/containerd/blob/main/docs/garbage-collection.md) — lease lifecycle, `gc.expire` TTL, reachability-only model
- [containerd issue #6583](https://github.com/containerd/containerd/issues/6583) — size-based eviction still unimplemented
- [PBS Maintenance Docs](https://pbs.proxmox.com/docs/maintenance.html) — atime-based two-phase GC, cutoff calculation, in-flight grace period
- [Nix Pills — GC](https://nixos.org/guides/nix-pills/11-garbage-collector.html) — symlink-root reachability model
- [Bazel issue #5139](https://github.com/bazelbuild/bazel/issues/5139) — eviction as long-standing unresolved gap
- [bazel-remote eviction docs](https://the-pi-guy.com/blog/managing_bazels_build_cache_and_eviction_policies/) — LRU + `max_size` + `max_age` composition
- [Go Modules Reference](https://go.dev/ref/mod) — all-or-nothing `go clean -modcache`
- [OCI Specs v1.1 Release](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/) — referrers API, untagged artifact GC protection
- [noatime/relatime performance notes](https://opensource.com/article/20/6/linux-noatime) — atime unreliability
- `.claude/artifacts/research_content_addressed_storage.md` — prior OCX CAS research
- `.claude/artifacts/adr_three_tier_cas_storage.md` — accepted OCX three-tier CAS ADR
