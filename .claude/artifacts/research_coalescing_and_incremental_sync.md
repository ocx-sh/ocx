# Research: Request Coalescing & Incremental Index Sync

Scope: design-pattern survey for (A) missing request coalescing in the index
source caches, and (B) the reverted per-package root-digest diff. Grounded
against the current tree: `crates/ocx_lib/src/utility/singleflight.rs` (the
existing `Group<K,V>` primitive, used in `chained_index.rs`, `pull.rs`,
`pull_local.rs`) and the uncoalesced read-check-fetch-insert caches in
`crates/ocx_lib/src/oci/index/ocx_index.rs::check_format_version` (line 774)
and `::resolve_root` (line 809).

---

## 1. Singleflight / request coalescing in async Rust — survey

| Option | Coalesces? | Also caches? | Error handling | Cancel safety | Extra dep? |
|---|---|---|---|---|---|
| **This repo's `singleflight::Group`** | Yes (watch-channel) | Yes, for group lifetime | `Handle::fail` broadcasts a cloned `SharedError` to every waiter; `Resolved` retained forever incl. negative results | `Handle::drop` broadcasts `Error::Abandoned` to **every** waiter — nobody silently retries | No |
| `moka::future::Cache::{get_with,try_get_with}` | Yes, by design | Yes (full LRU/TinyLFU cache w/ eviction) | `try_get_with`: **error is not cached** — entry stays absent, next caller retries the init future | Not documented for leader-drop; eviction policy adds complexity you don't want for 3 static fields | Yes |
| `tokio::sync::OnceCell` per key | Only after the key's cell exists | Yes, once set | N/A (no typed error slot) | If init is cancelled/panics, **another waiter is silently promoted to retry** — no shared error, no "abandoned" signal | No (tokio) |
| `dashmap` + `entry()` API | **No**, for async work | Yes, for the map part | N/A | N/A — the deadlock risk is the actual finding | Already a dep? check |
| `futures::future::Shared` | Yes, for one future | No (needs a `HashMap<K, Shared<...>>` wrapper you write yourself) | `Shared` clones the `Output`, so `Output` must be `Clone` — same constraint as `Group<K,V>` | Dropping all clones before completion cancels the inner future; a late arrival after that sees nothing — you'd reinvent leader-tracking | No (futures) |
| `async_singleflight` / `singleflight-async` (crates.io) | Yes | No (single-shot per call, not a cache) | Closure-based — you build the future *before* knowing if you're leader | Crate-specific; not audited here | Yes |

**Why `dashmap::entry()` doesn't solve this at all.** The entry API returns a
guard backed by a synchronous shard lock. Holding that lock across an
`.await` (to run the actual fetch) risks a real deadlock — DashMap's own
docs warn "may deadlock if called when holding any sort of reference into
the map," and this is a commonly-hit footgun in async code
([Beware of the DashMap deadlock](https://gnunicorn.org/writings/beware-of-the-dashmap-deadlock/),
[dashmap#79](https://github.com/xacrimon/dashmap/issues/79)). So the realistic
pattern is: lock, check, drop the lock, `await` the fetch, lock again, insert
— which is exactly the racy read-check-fetch-insert shape already in
`ocx_index.rs`. DashMap does not buy coalescing here; it only makes the map
mutation itself thread-safe.

**Why `moka` is the wrong fit despite "coalescing by design."** Moka's
`try_get_with` is built around *retryable* transient failures: a failed init
leaves the key absent so the next caller tries again
([moka::future::Cache docs](https://docs.rs/moka/latest/moka/future/struct.Cache.html)).
That's backwards for this codebase's requirement: a **confirmed 404 is a
positive, load-bearing result** that must be cached and broadcast, not
discarded. `ocx_index.rs` already draws this exact line explicitly in its own
doc comments — an *unconfirmed* 404 (transport failure, unsupported version)
must never be cached, but a *confirmed* one is "memoized like a hit." Moka
optimizes for the opposite default and you'd have to fight the library (wrap
`Option<T>` as the success type, treat "confirmed absent" as `Ok(None)`) to
get there — at which point you've re-derived the repo's own `Acquisition`
enum, worse, with an eviction policy you don't need.

**Why `tokio::sync::OnceCell` fails the exit-code-parity requirement.** If the
leader's init future is cancelled (e.g. the owning task is aborted), one of
the waiting tasks is silently promoted to redo the init — per-cell, this is
documented tokio behavior. That waiter may now hit a *different* failure mode
(or succeed) than the original leader would have, so two callers of the same
logical operation can walk away with different exit codes for what should be
one shared outcome. The repo's own `Handle::drop` → `Error::Abandoned`
broadcast avoids this entirely: every waiter sees the same abandonment, maps
through the same `ClassifyExitCode` path (`TempFail` is not returned for
abandonment — it's `Failure`, deliberately not retried blindly).

**Recommendation.** Don't adopt a library. `Group<K,V>` already satisfies both
hard constraints the task calls out — negative-result caching (`Resolved`
retains `None` for group lifetime) and exit-code parity (`Abandoned`
broadcasts identically) — and it's already proven in 3 call sites. Wire the
same `Group` into `OcxIndex`'s `config` and `roots` caches: one
`Group<(), Arc<IndexFormatConfig>>` (or a plain `OnceCell`-shaped group with a
unit key, since there's only one config per index instance) for
`check_format_version`, and one `Group<String, Option<Arc<IndexRoot>>>` for
`resolve_root`, mirroring `chained_index.rs:200`'s
`Group<String, Option<(Digest, Manifest)>>` almost exactly. This is rung 2 of
the ladder (reuse what's in the codebase), not rung 5 (new dependency).

Sources: [moka::future::Cache](https://docs.rs/moka/latest/moka/future/struct.Cache.html), [tokio::sync::OnceCell](https://docs.rs/tokio/latest/tokio/sync/struct.OnceCell.html), [Beware of the DashMap deadlock](https://gnunicorn.org/writings/beware-of-the-dashmap-deadlock/), [dashmap#79](https://github.com/xacrimon/dashmap/issues/79), [dashmap#253 key-level guarantees](https://github.com/xacrimon/dashmap/issues/253).

---

## 2. Is singleflight the right tool for a cold-cache stampede, or is it sequencing?

Two genuinely different bug shapes, and the fix differs:

- **Genuine race** (N concurrent callers, same key, no ordering relationship
  between them): singleflight is the correct tool. There is no "before" to
  sequence — the callers are concurrent by construction (64-wide fan-out
  hitting the same package/tag). This is what `Group` is for, and it's the
  shape `chained_index.rs` and `pull.rs` already handle correctly.
- **Sequenced fetch that failed to populate the cache**: singleflight is the
  *wrong* diagnosis, and adding it would paper over a real bug. If caller A
  is guaranteed to run before callers B..N (e.g. a resolve pass that must
  complete before dependents fan out), and the cache is still empty when B
  arrives, the actual defect is that A's fetch path has a branch that returns
  without writing to the cache — a missing `insert` on some branch, an early
  `return` before the `self.cache.write().await` line, or a scope where the
  "warm" step and the "read" step disagree on the key. Singleflight would
  hide this: B would coalesce onto... nothing, because A already finished and
  didn't register as a leader for the key B needs. The fix is to grep every
  return path of the sequenced populator and confirm each one that should
  cache does, i.e. root-cause the omission, not bolt on concurrency control
  for a problem that isn't concurrent.

**Rule of thumb**: singleflight fixes "many readers show up before the write
happens." It cannot fix "the writer showed up and up left without writing."
Sequencing bugs need a code-path audit (which branch skips the insert);
concurrency bugs need `Group`. Since the task states one instance of each was
found, treat them as two separate diagnoses, not one fix applied twice.

Sources: [Cache stampede — Grokipedia](https://grokipedia.com/page/Cache_stampede) (thundering-herd/dogpile terminology, singleflight as the standard concurrent-miss fix), general pattern also documented in [oneuptime: Redis cache stampede](https://oneuptime.com/blog/post/2026-01-21-redis-cache-stampede/view).

---

## 3. How other package managers do incremental index sync

This is the section with the actual new idea (§3.6) — read that part even if
you skip the survey.

### 3.1 Debian `apt` — `InRelease` + pdiffs
`InRelease` is a signed, single-file manifest listing every metadata file's
hash. `Packages` gets incremental updates via `.pdiff` files: ed-style diffs
applied in sequence to the last-known `Packages` file, downloading ~15-30KB/day
instead of re-fetching a multi-MB file
([apt with index diff support](https://lists.debian.org/debian-devel/2005/09/msg00494.html),
[DebianRepository/Format](https://wiki.debian.org/DebianRepository/Format)).
**Invariant that makes it valid**: the server-side `Packages` file is
*append/mutate-only from the server's own perspective* — the client never
merges its own edits into it. A pdiff chain is valid precisely because the
client's copy is a byte-for-byte mirror of some prior server state, never a
locally-authored variant. Diffing is comparing two points on the *same*
timeline.

### 3.2 Fedora `dnf` — `repomd.xml` + zchunk
Metadata is content-defined-chunked (zchunk/`zck`); `repomd.xml` advertises
`_zck` variants, and the client re-downloads only the chunks whose content
changed, reported at up to 95% savings per update
([Changes/Zchunk Metadata](https://fedoraproject.org/wiki/Changes/Zchunk_Metadata)).
**Invariant**: same as apt — chunking operates on a byte-identical mirror.
The clever part is chunk-level content addressing (dedup below the whole-file
granularity) rather than the diff being *semantic*; it still assumes local
bytes are a subset/history of server bytes, never independently edited.

### 3.3 Nix — narinfo / binary cache
Nix doesn't really have an "index sync" problem at all: store paths are
content-addressed (`/nix/store/<hash>-name`), and a client asks a binary
cache "do you have `.narinfo` for this exact hash?" — a point HEAD-style
query, not a document diff
([Nix Binary Caches](https://hackmd.io/@NeqoUxq9SYSXDC7wNwihSA/S1zUW06lj)).
**Invariant**: the hash *is* the identity, so there is nothing to
reconcile — either the path exists under that hash or it doesn't, and if it
exists its content can never legitimately differ (it's derived
deterministically from its inputs). This sidesteps the whole "has this
document changed" question by making the question always about an immutable,
uniquely-named object instead of a mutable document.

### 3.4 `crates.io` — sparse HTTP index
RFC 2789 replaced the git-clone-the-whole-index model with per-crate JSON
files fetched over plain HTTPS, letting ordinary HTTP caching (ETag /
conditional GET) and CDNs do the incremental work
([RFC 2789](https://rust-lang.github.io/rfcs/2789-sparse-index.html)). The
broader argument — that git is a bad fit for what is fundamentally a
key-value point-lookup workload — is made at length in
[Package managers keep using git as a database, it never works out](https://nesbitt.io/2025/12/24/package-managers-keep-using-git-as-a-database.html),
which traces the same failure across Cargo, Homebrew, CocoaPods, and vcpkg,
and in the mirroring survey
[Package Manager Mirroring](https://nesbitt.io/2026/03/20/package-manager-mirroring.html).
**Invariant**: each crate's file is *itself* append-only (new versions are
new lines/entries; existing entries are immutable other than the rare
yank-flag flip), so a conditional GET against a single small file is cheap
and correct — you're asking "has this specific, mostly-append-only document
changed," not diffing content that a *reader* also mutates.

### 3.5 Homebrew & Go module proxy
Homebrew moved formula/bottle discovery to a JSON API plus OCI-artifact
bottles rather than a git clone of `homebrew/core`
([Package Manager Mirroring](https://nesbitt.io/2026/03/20/package-manager-mirroring.html)).
Go's module proxy (`@v/list`, `@v/<version>.info`, `@latest`) works because
**published module versions are immutable by protocol contract** — re-pushing
an existing tag is rejected outright, so every response except `@latest`
(and `@v/list`) can be cached forever, and even `@latest` is cheap to always
fetch fresh because it's a single small file, not because anyone diffs it
([Go Modules Reference](https://go.dev/ref/mod), immutability discussion in
[Grab: Go module proxy](https://engineering.grab.com/go-module-proxy)).

### 3.6 The actual invariant, and why it doesn't transfer to OCX — plus what does

Every one of the above (apt, dnf, Nix, crates.io, Go) relies on some version
of the same premise: **the local copy, where one exists, is a mirror of a
past server state — never independently edited.** Diffing (pdiffs, zchunk),
content-addressing (Nix), and conditional GETs (crates.io, Go) are all just
different-granularity ways of asking "what changed on the *server's own
timeline* since the point my mirror is at." None of them have a concept of a
client that *writes into* the same document it's syncing.

That's exactly the shape that broke the reverted diff: OCX's local root is
**merged** — union of remote tags plus locally-held tags that are never
dropped — so it is not a point on the remote's timeline at all. Comparing
`sha256(local_root)` to `sha256(remote_root)` is comparing two different
documents that happen to overlap, not two versions of one document. No
amount of cleverer diffing (rsync rolling checksums, structural diff,
merkle-tree-of-fields) fixes this, because the premise (comparable
timelines) is false, not the diff algorithm.

**Is there a pattern for a locally-authored/merged document?** The honest
answer: not one that make the *content* comparison valid — the two
namespaces don't converge, so there's nothing to make convergent. But there
is a real, different lever that all these systems also use underneath their
specialized formats, and it composes cleanly with a merged local document
because it never compares the merged bytes at all:

**Ask "did upstream change," not "does my merged copy match upstream."**
apt's `InRelease` carries a `Date:` field and its own hash so a client can
cheaply learn "nothing changed since my last fetch" before touching
`Packages` at all; crates.io's and Go's HTTP layers ride on plain
`ETag`/`If-None-Match` or `Last-Modified`/`If-Modified-Since` conditional
requests for the same purpose. Applied here: fetch (or condition-check) the
**remote's own root document only**, independent of the local merged
document, and treat "remote unchanged since last fetch" (via ETag, a
remote-side revision counter, or the remote's own `sha256(remote_root)`
compared to the *last remote root you fetched* — not the local merged one) as
license to skip re-parsing/re-merging for that package. This is a genuinely
different comparison than the reverted one: old-remote vs. new-remote, never
local vs. remote. It requires caching "last-seen remote root digest" as a
*separate* field from "current local merged root" — a small, honest
side-channel, not a re-derivation of the local document's meaning. This
reduces the fixed per-package cost of a no-op *upstream* refresh (the
"nothing changed on the server" case, which is presumably the common case in
steady state) without ever touching the "my local tags differ from remote's
by design" problem, which stays correctly unsolved because it isn't a bug.

Whether the current transport (`oci-client` / registry HTTP) already
surfaces conditional-request support (`ETag`, `If-None-Match`) for the
relevant endpoints is a separate, concrete question worth one grep before
committing to this — OCI registries vary in ETag support on manifest/blob
GETs, and the fallback ("last-seen remote digest, fetched unconditionally
but only compared to itself") still works without registry cooperation, just
without the bandwidth win.

Sources: [RFC 2789: sparse index](https://rust-lang.github.io/rfcs/2789-sparse-index.html), [Package managers keep using git as a database, it never works out](https://nesbitt.io/2025/12/24/package-managers-keep-using-git-as-a-database.html), [Package Manager Mirroring](https://nesbitt.io/2026/03/20/package-manager-mirroring.html), [Changes/Zchunk Metadata](https://fedoraproject.org/wiki/Changes/Zchunk_Metadata), [Nix Binary Caches](https://hackmd.io/@NeqoUxq9SYSXDC7wNwihSA/S1zUW06lj), [Go Modules Reference](https://go.dev/ref/mod), [DebianRepository/Format](https://wiki.debian.org/DebianRepository/Format).

---

## 4. Content-addressed skip — verify vs. trust

The surviving optimization ("don't fetch an object whose digest we already
hold") is standard practice — OCI/Docker clients skip re-pulling a layer once
its digest is confirmed present locally, using a single HEAD-equivalent
manifest check rather than re-transferring bytes
(container-registry pull-optimization pattern documented in
[OCI image pull mechanics](https://www.douglashellinger.com/explainer/container-oci-registry/pull-a-public-container-image/)).
The open question is never "should we skip the fetch" — it's "how do we know
the local bytes still match the digest we're trusting," and the answer
differs by how paranoid the system needs to be.

**The trust spectrum**, cheapest to most expensive:
1. **Existence-by-path** (does the file exist at the content-addressed
   path?) — zero cost, zero corruption detection.
2. **Metadata check** (size, mtime) — catches truncation, not bit flips.
3. **Full re-hash on every access** — catches everything, but this is
   exactly the fixed cost you're trying to eliminate; re-hashing an object
   every time you'd otherwise skip fetching it defeats the optimization.
4. **Periodic out-of-band verification, decoupled from the hot path** —
   ZFS's model: every *read* actually does re-verify checksums against the
   stored value as a matter of course, but that's affordable because ZFS
   checksums are cheap block hashes on a local device; the deliberate
   *scrub* is the heavier, explicitly periodic pass that walks every block
   including ones no read has touched recently, precisely because corruption
   can occur silently between reads
   ([Understanding ZFS Scrubs and Data Integrity](https://klarasystems.com/articles/understanding-zfs-scrubs-and-data-integrity/)).
   Git draws the same line: loose/pack objects are trusted by hash-derived
   path on every read; `git fsck` is the explicit, opt-in, out-of-band
   integrity walk.

**Your stated constraint sharpens this**: the local store only self-heals
*on the write path*. That rules out option 1 outright as a *permanent*
policy, not just a slow one — a bit-rotted object under a trusted path with
no read-time check and no periodic scrub is corrupt **forever**; nothing in
the system will ever notice, let alone repair it, because the only self-heal
trigger (a write) will never fire for an object nobody is writing again.
That's a strictly worse failure mode than "slow," it's "silently wrong,
indefinitely."

**Recommendation**: keep the hot path at option 1 (trust the path — that's
the whole point of content-addressed skip, and re-hashing on every check
just re-invents the fetch cost you removed), but pair it with an *explicit,
separate, low-frequency* scrub — a `ocx package verify`-style pass or a
background job that re-hashes a bounded sample of the store and re-triggers
the existing write-path self-heal (re-fetch-and-overwrite) for anything that
fails — rather than either (a) re-hashing inline on every skip-check, which
cancels the optimization, or (b) never checking at all, which strands
corruption permanently per your own stated constraint. This is the same
shape as ZFS scrub / `git fsck`: verification is real but it's a scheduled
job, not a per-access tax.

Sources: [Understanding ZFS Scrubs and Data Integrity](https://klarasystems.com/articles/understanding-zfs-scrubs-and-data-integrity/), [zpool-scrub(8)](https://openzfs.github.io/openzfs-docs/man/master/8/zpool-scrub.8.html), [OCI image pull mechanics](https://www.douglashellinger.com/explainer/container-oci-registry/pull-a-public-container-image/).

---

## Bottom line

1. **Coalescing**: extend the existing `singleflight::Group` into
   `OcxIndex::check_format_version` / `::resolve_root` — no new dependency,
   already proven to satisfy negative-caching and exit-code-parity
   constraints that rule out moka/OnceCell/dashmap/Shared.
2. **Stampede diagnosis**: the two found duplicates are different bugs —
   audit the sequenced populator's return paths for a missing cache-insert;
   apply `Group` only to the genuine-race one.
3. **Incremental sync**: no diff/hash scheme salvages the reverted approach —
   every real-world incremental-sync pattern assumes the local copy is a
   mirror, not an authored merge, and OCX's local root is authored by
   design. The one lever that survives is checking whether the *remote* root
   changed since last fetch (conditional GET / remote-side revision, never
   compared against the local merged bytes) — reduces fixed per-package cost
   without resurrecting the false "unchanged" premise.
4. **Content-addressed skip**: trust the path on the hot path (don't re-hash
   per access — that's the cost you removed), add a separate periodic
   scrub/repair pass so the write-path-only self-heal constraint doesn't let
   corruption strand forever.

---

# Orchestrator adjudication (2026-08-22)

Two findings above are ACCEPTED, two are REJECTED against project evidence the
researcher was not briefed on. Recorded here so the claims travel with their verdicts.

## §1 Coalescing — ACCEPTED
Use the repo's own `utility/singleflight.rs`, not moka/dashmap/OnceCell/Shared.
Agrees independently with disc-arch's Discover finding. Wire into
`OcxIndex::check_format_version` and `::resolve_root`, same shape as
`chained_index.rs:200`.

## §2 The two stampedes are different bugs — ACCEPTED
Matches the Discover trace exactly:
- **Sequenced case (A4)** — `fetch_root_document` fetches the root and never populates
  `cache.roots`, so the fan-out that follows always misses. Fix = populate the cache.
  Singleflight would *mask* this rather than fix it, as the researcher notes.
- **Genuine race (A8)** — `resolve_root` / `check_format_version` read-check-then-fetch
  under a 64-wide fan-out. Fix = `Group`.

## §3.6 "Cache a last-seen remote root digest + conditional GET/ETag" — REJECTED
Presented as a new idea. It is the exact design this project implemented and reverted,
prohibited twice over:

1. `adr_index_indirection.md:572-573` — **"Amended 2026-07-30 — the conditional GET is
   retired."** The clause specified an `If-None-Match` request "whose validator was
   persisted as `c/index.json.etag`". Retired because the sidecar "was the only file in an
   index tree neither served by the index site nor content-addressed", and because it
   bought a 304 over a 200 for a kilobyte document with the round trip paid either way.
2. `adr_index_indirection.md:1073` — restoring the diff "needs a recorded
   last-observed-remote digest, and **every** home for one re-introduces mirroring one
   field at a time — in the catalog envelope, **in a sidecar (the `.etag` file's category,
   already rejected)**, or in machine-global state that desyncs from a shipped index home."

There is also a **live test that deletes such sidecars**:
`index_store.rs:2269 commit_removes_a_stale_etag_sidecar_left_by_an_older_ocx`. Shipping
this would fight a test written specifically to clean it up.

Worth recording: an independent researcher, reasoning from general package-manager prior
art, re-derived the precise design the ADR rejects. That is the second time in this run
(the orchestrator did it first). The ADR's reasoning is correct but evidently
non-obvious — the plan's ADR should restate it prominently.

## §4 "Trust the path; re-hashing per check cancels the optimization" — REJECTED
The cost premise is wrong, and it contradicts disc-store's Discover finding.

The alternative to a local hash is **fetching the object over the network, and then hashing
it anyway** — `write_verified_object` recomputes `sha256(bytes)` on every write as a
CWE-345 trust-boundary check. So hash-verifying a local copy costs *strictly less* than the
path it replaces, at every object size:

| | Fetch path (today) | Gated path (A3) |
|---|---|---|
| Network | full object transfer | none |
| Hash | yes (`write_verified_object`) | yes (`read_dispatch_object`) |

`MAX_INDEX_DOCUMENT_BYTES = 32 MiB` is a DoS bound, not an expected size — a real OCI image
index for a handful of platforms is single-digit KB. Even at the cap, hashing locally beats
transferring 32 MiB through a proxy and then hashing it.

The proposed **periodic scrub/repair pass** is therefore unnecessary as well as YAGNI: the
gated path already self-heals, because `Err(DigestMismatch)` routes to "fetch" and the
fetch's `write_dispatch_object` overwrites the corrupt file. No new subsystem is needed.

**Standing design (disc-store's, unchanged):** gate on `read_dispatch_object` —
`Ok(Some(_))` skip, `Ok(None)` fetch, `Err(DigestMismatch)` fetch. Never `Path::exists()`.
