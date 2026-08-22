# ADR: `ocx index sync` — Performance and Resilience Without Remote Memory

## Metadata

**Status:** Proposed — awaiting owner ratification.
**Date:** 2026-08-22
**Deciders:** mherwig
**Originating issue:** [ocx-sh/ocx#330](https://github.com/ocx-sh/ocx/issues/330) — `ocx index sync` failover
**Related issues:**
[#324](https://github.com/ocx-sh/ocx/issues/324) (net: one HTTP transport owner — ARCH-16 foundation unit),
[#333](https://github.com/ocx-sh/ocx/issues/333) (deferred: `--jobs` into the index fan-out, `[registry]` config),
[#288](https://github.com/ocx-sh/ocx/issues/288) (settled doctrine: explicit whole-source sync),
[#314](https://github.com/ocx-sh/ocx/issues/314) / [#319](https://github.com/ocx-sh/ocx/issues/319) (fetch-discard-refetch, per-candidate-not-per-run),
[#316](https://github.com/ocx-sh/ocx/issues/316) / [#167](https://github.com/ocx-sh/ocx/issues/167) (unbounded fan-out; the separate-permit-class rule),
[#270](https://github.com/ocx-sh/ocx/issues/270) (fork — retry a transient chunk PATCH; adjacent to A5)
**Tech Strategy Alignment:**
- [x] Follows the Golden Path in `.claude/rules/product-tech-strategy.md` — **no new dependency**. The
  coalescing primitive is the repo's own `utility/singleflight.rs`; the retry ladder is hand-rolled over
  the existing `reqwest`/fork clients (see "Don't Own Non-Domain Code" check in §11).
**Domain Tags:** oci, index, auth, transport, performance, resilience
**Depends on:**
[`adr_index_indirection.md`](./adr_index_indirection.md) (wire grammar; the 2026-08-05 lock amendment; the
2026-07-30 conditional-GET retirement),
[`adr_oci_index_only_dispatch.md`](./adr_oci_index_only_dispatch.md) (D1/D2/D6),
[`design_spec_servable_index_snapshot.md`](./design_spec_servable_index_snapshot.md) (C-012/C-013/C-014/C-023/C-024/C-027)
**Supersedes:** nothing. **Superseded by:** N/A.

---

## 0. The three constraints this ADR is written inside

Stated first because two independent parties on this initiative — the orchestrator, then an outside
researcher reasoning from apt/dnf/crates.io/Go prior art — re-derived a design the project has already
implemented, shipped and reverted. The reasoning is correct and evidently non-obvious, so it is restated
here rather than cited.

### 0.1 No stored last-observed-remote state, in any home

> `adr_index_indirection.md:1069-1075` — "Under merge semantics the local root's bytes legitimately
> diverge from the remote's (local-only tags), so the old digest diff would report every package as
> permanently stale; restoring it needs a recorded last-observed-remote digest, and **every** home for one
> re-introduces mirroring one field at a time — in the catalog envelope, in a sidecar (the `.etag` file's
> category, already rejected), or in machine-global state that desyncs from a shipped index home."

> `adr_index_indirection.md:572-581` — "**Amended 2026-07-30 — the conditional GET is retired.** This
> clause specified an ETag `If-None-Match` request whose validator was persisted as `c/index.json.etag`, a
> sibling of the catalog. That sidecar was the only file in an index tree neither served by the index site
> nor content-addressed … It bought a `304` over a `200` … for a catalog measured in kilobytes, with the
> round trip paid either way."

There is a live test that **deletes** such sidecars:
`index_store.rs::commit_removes_a_stale_etag_sidecar_left_by_an_older_ocx`. Any ETag/If-None-Match /
last-seen-remote-digest scheme would ship against a test written specifically to clean it up.

**Consequence for this ADR: every performance win here is derived from state OCX already holds for its own
reasons — the content-addressed `o/` store, the in-process caches, the token cache — never from a new
record of what the remote looked like last time.** The one place a validator-shaped comparison appears (A3,
A12) compares a digest the *committed root already carries* against bytes *on local disk*. Nothing is
recorded to enable it, and nothing new is written when it fires.

### 0.2 A pin moves only for something the invocation named

> `adr_index_indirection.md:1043-1044` — "A pin moves **only** for a package the user explicitly names …
> Never as a side effect of an operation about something else."

`subsystem-oci.md` restates the practical test: *"name the command the user ran, and the package they
named. If the diff can move a pin (a tag's `content`, or a root's `repository`) for anything outside that
set, it is wrong however well-motivated the fetch is."* Every decision in §5 carries its own row in the
pin-safety table (§9), and each row states the mechanism, not an assurance.

### 0.3 Determinism survives

`ocx package install|exec <pkg>:<tag>` twice gives the same result. No change here may make the answer
depend on cache warmth, on completion order in a fan-out, or on whether a retry fired.

---

## 1. Context — what is actually slow, measured

Discovery traced the request path end to end and corrected three claims that were in the issue framing.
Load-bearing findings, each verified against source by direct read (the command proxy in this repo mangles
grep output and has fabricated line numbers; nothing below rests on a grep):

### 1.1 The fork re-authenticates on every registry operation

`Client::auth()` (`external/rust-oci-client/src/client.rs:856-885`) calls `store_auth_if_needed`, then
runs `_auth()` **unconditionally**. There is no `self.tokens.get(...)` read anywhere in the function; the
comment `// preserve old caching behavior` refers only to the `tokens.insert()` afterwards. The sibling
`get_auth_token()` (`:451-466`) is the one that consults the cache, and it is reached only from
`RequestBuilderWrapper::apply_auth` — the header-attach path, not the pre-flight.

Per-call cost of `_auth()`:

| `RegistryAuth` | Requests per `ensure_auth` |
|---|---|
| `Bearer(token)` | **0** — returns at the `if let` before `GET /v2/` |
| `Anonymous`/`Basic`, no `WWW-Authenticate` | **1** (`GET /v2/`) |
| `Anonymous`/`Basic`, bearer challenge (Docker Hub, GHCR, quay.io, ACR) | **2** (`GET /v2/` + `GET {realm}?scope=…`) |

So on a bearer-challenge registry with Basic or anonymous credentials: **3 requests per registry
operation, 2 of them wasted, warm or cold, forever.** The comment at `crates/ocx_lib/src/oci/client.rs:2181-2182`
("it is a no-op on cache hit") is false.

This is **wider than index sync**: `pull_layer_with_caps` calls `ensure_auth` once per *layer*, so a
five-layer package pull burns ten wasted requests on re-auth alone.

`get_auth_token` is also **not coalesced** — a plain read-check under `RwLock`. N concurrent callers on one
`(registry, repository, operation)` key while cold each miss and each run `_auth()`.

### 1.2 The refresh path fetches the same root twice, then never skips anything

`refresh_tags` (`local_index.rs:193`) branches on `fetch_root_document`, which **never touches
`self.cache`** (`ocx_index.rs:1169-1188` — it goes straight to `transport.get`). The fan-out that follows
calls `persist_dispatch` → `fetch_manifest_raw_bytes` → `resolve_root`, and `resolve_root`
(`ocx_index.rs:805-826`) is a read-check-then-fetch against `cache.roots` that the first call always
misses. Under `buffer_unordered(TAG_REFRESH_CONCURRENCY = 64)` up to `min(distinct_tags, 64)` concurrent
tasks each issue the *same* redundant root `GET`, on top of the one `fetch_root_document` already paid.

`persist_dispatch` (`local_index.rs:378-398`) then calls `fetch_manifest_raw_bytes` **unconditionally** —
no presence check. An unchanged tag pointing at an already-persisted `o/` object is refetched on every
sync, forever, on both provenance kinds.

Request formulas today (R registries, P packages, T tags, D distinct dispatch digests ≤ T):

- **Published:** `R×[1 config + 1 catalog] + R×P×[1 root + min(D,64) redundant roots + D dispatch]`
- **Derived:** `R + R×P×(T+1)`

### 1.3 One failing tag discards a whole package's completed work

`refresh_published` (`local_index.rs:255-269`) ends its fan-out with `try_collect::<()>()`. The first `Err`
drops the stream — cancelling every in-flight future in the 64-wide window — **and** returns before
`commit_published_root` runs. Every tag that already succeeded in that call is lost: its dispatch object
sits in `o/` as a harmless orphan while the root is never updated to point at it. `refresh_derived`
(`:308-327`) has the same shape via `try_filter_map` + `try_collect`.

Cross-*package* partial success is already the shipped contract (C-012 Aggregation: "Successful packages
keep their tags"; acceptance test
`test_a_refresh_failure_leaves_the_packages_that_did_refresh_pinned`). Within a package it is not.

### 1.4 No retry anywhere on the index-document path

`ReqwestIndexTransport::get` (`ocx_index.rs:219-286`) has no retry and no backoff. `oci/client/builder.rs`
says it in as many words for the other half: *"nothing on the pull path retries"*.

Retries **do** exist elsewhere, and #324's "three copies of a 1s→30s ladder" is overstated. The honest
inventory:

| Site | Shape |
|---|---|
| `native_transport.rs::do_push_blob` | `PUSH_RETRY_ATTEMPTS=2`, initial 1s, ×2 → **1s, 2s**. `RegistryTransient` only. No jitter. |
| `project/resolve.rs::retry_fetch` | `DEFAULT_RETRY_ATTEMPTS=2`, initial 1s, ×2 → **1s, 2s**. `DEFAULT_PER_TOOL_TIMEOUT=30s` is a *total deadline over the chain*, not a backoff cap — this is where #324's "30" comes from. |
| `forge/poll.rs` | initial 2s, ×2 `.min(30s)`, deadline 300s, per-probe 8s. Different domain (GitHub readiness). |
| `forge/github.rs` | `GIT_DATA_RETRY_DELAYS = [3s, 9s, 27s]` — a fourth distinct shape. |
| `forge/gitlab.rs:66,750` | `COMMIT_RETRY_DELAYS = [3s, 9s, 27s]` — byte-identical to github's, same `for delay in …` loop. |

**Five sites, in two duplicate pairs**: `[1s, 2s]` twice (`do_push_blob`, `retry_fetch`) and
`[3s, 9s, 27s]` twice (`forge/github.rs`, `forge/gitlab.rs`), plus `forge/poll.rs` on its own. So the
duplication #324 complains about is **worse** than #324's own framing, not overstated — the correction
strengthens the case for a shared seam rather than weakening it. This ADR builds that ladder, but sized
from the evidence in §5.5 rather than from #324's premise.

### 1.5 The timeout asymmetry is backwards for the failing environment

- **Index** (`ocx_index.rs:93-99`): `INDEX_CONNECT_TIMEOUT = 30s` + `INDEX_REQUEST_TIMEOUT = 60s` mapped to
  reqwest `.timeout()` — a **hard total-request deadline**, plus `redirect::Policy::none()`.
- **Registry** (`oci/client/builder.rs:71-104`): `REGISTRY_READ_TIMEOUT = 120s` mapped to
  `.read_timeout()` — a per-frame **idle** bound, plus `REGISTRY_CONNECT_TIMEOUT = 30s`.

Index documents are small and the index path is the one more likely to sit behind a throttling corporate
proxy, yet it is the one carrying the hard cap. curl's own guidance (`--max-time` vs
`--speed-limit`/`--speed-time`) and Go's `net/http` `Client.Timeout` caveat both land on the same rule:
use an idle/stall detector for a slow-but-progressing link, with a generous total cap as backstop. The
current split is the inverse.

### 1.6 The environment #330 fails in

Cross-vendor proxy documentation (Zscaler, Juniper, Palo Alto, Fortinet, Broadcom) agrees that non-browser
HTTP/2 is **commonly downgraded to HTTP/1.1** under TLS inspection. reqwest's
`pool_max_idle_per_host` defaults to `usize::MAX` and there is no client-side cap on concurrent in-flight
connections. So OCX's own 512 ceiling is the only thing between a sync and 512 concurrent CONNECT tunnels,
each with its own TLS handshake — doubled in CPU cost on the proxy, which terminates both legs.

Docker's own daemon reference ships `--max-concurrent-downloads` with a **default of 3**, and its docs say
plainly: *"If you are on a low bandwidth connection this may cause timeout issues and you may want to lower
this."* Harbor's proxy cache does **not** coalesce concurrent cache misses
([goharbor/harbor#22570](https://github.com/goharbor/harbor/issues/22570)) — client concurrency on a cold
cache is *multiplied* upstream, not absorbed.

**This ADR does not change the concurrency ceiling.** §7 records why, and what evidence would reverse that.

---

## 2. Problem statement

`ocx index sync <REGISTRY>...` against a corporate registry behind a TLS-intercepting proxy is slow, and a
single transient failure loses work that already succeeded. Four independent causes, all measured above:

1. **Multiplied requests** — 2 wasted auth requests per operation (§1.1), 1+min(D,64) redundant root GETs
   per package (§1.2), a full dispatch-object refetch for every unchanged tag (§1.2).
2. **No resilience** — zero retries on the index path (§1.4); one failed tag discards a package's whole
   completed refresh (§1.3).
3. **Wrong timeout shape** for a slow-but-progressing link (§1.5).
4. **Unbounded fan-out** in `index_catalog.rs --tags` (one `JoinSet` task per repository, no cap).

---

## 3. Options considered

Weighted criteria. Weights reflect what #330 is actually about: the run has to *finish* on a hostile
network, and it has to not lose work. Raw throughput on a healthy network is the least important axis.

| # | Criterion | Weight |
|---|---|---|
| K1 | Closes #330's failure mode (a sync completes behind a proxy) | 30% |
| K2 | Constraint safety — cannot violate §0.1/§0.2/§0.3, and the check is structural rather than a promise | 25% |
| K3 | Reversibility — how cheaply a decision is undone if it turns out wrong | 15% |
| K4 | Blast radius / review surface — how much unrelated code must move | 15% |
| K5 | Request reduction (the throughput win) | 10% |
| K6 | Forecloses nothing for #324 / #333 | 5% |

### Option A — Foundation unit first: one client factory + one retry ladder, then the items on top

Build #324's foundation *scoped to what A5/A6 need* — a shared hardening/timeout/retry seam — and land
A1a/A1b/A3/A4/A7/A8/A11/A12 against it.

- **What it costs.** The seam has to serve three genuinely different client types: two bare
  `reqwest::Client`s (`ocx_index.rs` primary + fallback, `forge/github.rs`), the Sigstore client
  (`oci/endpoint.rs`, which already shares `seed_embedded_roots` but not `harden`), and the fork's
  `native::ClientConfig` — which is **a different type and cannot share a `reqwest::ClientBuilder` at
  all**. So "one factory" is really "one policy object, two adapters", and the honest cost is designing
  that policy object before any of the ten items lands.
- **What it forecloses.** Nothing — but it front-loads a refactor that has to be right before any user-
  visible fix ships. On a branch this is a serialization point.
- **K1** high (the ladder is the fix). **K2** high — a single seam is one place to assert on. **K3**
  medium — a shared seam is harder to back out than a local `if`. **K4** high cost — touches four
  subsystems before fixing anything. **K5** neutral. **K6** best.

### Option B — Minimal surgical fixes in place

Each item lands where it lives: a retry loop inside `ReqwestIndexTransport::get`, a timeout swap inside
`build_index_http_client`, a cache write inside `fetch_root_document`, a `Group` inside `resolve_root`, a
`collect` inside `refresh_published`, a `Semaphore` inside `index_catalog.rs`.

- **What it costs.** A **fifth** retry ladder and a **fourth** hardening site, in a repo that already has
  four ladders and three hardened builders. #324 exists precisely because that duplication is the reported
  root cause of a TLS-proxy outage class.
- **What it forecloses.** It makes #324 strictly more expensive later — the consolidation then has five
  sites to reconcile instead of four, and one of them is brand new, which is the worst possible time to
  add one.
- **K1** high. **K2** medium — the retry policy would exist in two files with no shared assertion, and a
  "prove it can go red" check has to be written twice. **K3** best — every change is a small local revert.
  **K4** best. **K5** neutral. **K6** worst.

### Option C — Fork-first, then ocx-side

Land the fork's `auth()` cache consultation (A1a) + coalescing (A1b) first, bump the submodule, then do the
ocx-side items in a second pass.

- **What it costs.** Two landings, with a submodule bump between them. The fork's own test suite **is
  excluded from the ocx workspace and never runs in CI** (§10), so the fork half arrives unverified from
  ocx's perspective until the ocx-side wire test lands — which is in the second pass.
- **What it buys.** A1 is the single largest measured win (§1.1) and is *wider than index sync* — it fixes
  every layer pull too. Landing it alone is the highest value-per-line change in the whole set.
- **What it forecloses.** Nothing, but it leaves a window where the fork behaviour is unpinned from the
  ocx side.
- **K1** medium alone (fixes request count, not the failure mode). **K2** medium — the unpinned window is
  the risk. **K3** high per step. **K4** low. **K5** best. **K6** neutral.

### Option D (considered, rejected) — Reduce concurrency instead

Drop `INDEX_REFRESH_CONCURRENCY` from 8 toward Docker's 3, or make the product configurable, and ship
nothing else.

- Rejected as *this ADR's* answer because it is exactly the A9/A10 work the owner scoped to
  [#333](https://github.com/ocx-sh/ocx/issues/333), and because a fixed lower number trades a proxy
  symptom against every healthy network. Recorded in §7 with the evidence that argues for pulling it
  forward.

### Scored comparison

| | K1 (30) | K2 (25) | K3 (15) | K4 (15) | K5 (10) | K6 (5) | Total |
|---|---|---|---|---|---|---|---|
| **A** foundation-first | 5 | 5 | 3 | 2 | 3 | 5 | **4.05** |
| **B** surgical in place | 5 | 3 | 5 | 5 | 3 | 1 | **4.10** |
| **C** fork-first | 3 | 3 | 4 | 4 | 5 | 3 | **3.50** |
| **A′** scoped foundation (recommended) | 5 | 5 | 5 | 4 | 3 | 4 | **4.55** |

A and B score within noise of each other, which is the useful result: the scoring says the choice is not
between the two of them but between their *failure modes*.

**A′ is scored in the table rather than presented as an override of it.** Scoring the recommendation on the
same rubric is the honest form; a "failure-mode override" of a table the recommended option was never in
reads as a rationalisation even when the arithmetic is clean. A′ takes A's K1/K2 (same fix set), B's K3
(a value object at one call site is as inlinable as a local constant) and most of B's K4, and it dominates.

**And B's cost is smaller than a first draft of this section claimed.** D-010 puts the ladder in exactly
**one** place, this plan migrates none of the four existing ladders onto the seam, and §1.4 concedes those
four sit in genuinely different domains. Reversibility is symmetric — a value is trivial to inline back,
and named constants beside their one consumer are trivial to hoist out. So the honest statement of the
trade is *one new file versus none, with identical consolidation cost to #324 either way* — a small
decision, which is why the seam must not be paid for with a work package of its own (it ships in the same
commit as its first consumer).

### Recommendation — **A′: a scoped foundation unit, with C's ordering inside it**

Adopt Option A, with two amendments that take B's and C's strongest properties:

1. **Scope the foundation to A5/A6's actual needs** — a `RetryPolicy` + `TransportHardening` pair used by
   the index transport, with the fork client adapting the same *values* through `ClientConfig`. Do **not**
   carve `utility/tls.rs` / `oci/ssrf.rs` in this plan; those are #324's own scope and are not on any
   path #330 fails through.
2. **Sequence fork-first inside it** (C), because A1 is the largest win and is independent of the seam.

Rationale, stated as the trade this makes: we accept one serialization point (the policy object) to avoid
adding a fifth retry ladder to a codebase whose reported TLS-proxy outage was caused by exactly that
duplication. Every other item in scope is independent of the seam and parallelizable behind it.

**The recommendation is reversible at low cost.** If the policy object proves awkward, each consumer keeps
a local constant and the seam is deleted — the items themselves do not depend on it. That is why the seam
is deliberately a *policy value object*, not a client factory trait: a value is trivial to inline back.

---

## 4. Decisions — the shape of each item

Numbered `D-nnn`. Each states the change, the mechanism, and what must not happen.

### D-001 (A1a) — The fork's `auth()` consults the token cache before re-authenticating

`Client::auth()` keeps `store_auth_if_needed` (which is the load-bearing side effect — see D-002), then
**returns early when the token cache already holds a live entry for `(registry, repository, operation)`**,
skipping `_auth()` entirely.

**D-001a — every *caller-supplied* credential is excluded from the early return, not just `Bearer`.** `_auth()` returns the
*caller-supplied* bearer token before any network call (`client.rs:903-907`), and `auth()` then inserts it
into the token cache (`:866-871`). So `auth()` is today the only route by which a caller installs a
**rotated** bearer token for a key; a cache-first early return makes a re-`auth()` with a fresh token a
no-op and `apply_auth` keeps serving the stale one until its recorded expiry. Bearer costs **zero**
requests either way, so the early return buys nothing on that path and costs a correctness property.
C-001's edge case (a) as originally written could not catch this — "0 requests on both calls" is true
whether Bearer early-returns or falls through — so it is restated as a *staleness* assertion: `auth()` with
token A, then `auth()` with token B, then observe the wire header carries B. Not reachable through ocx
today (one credential per registry per process: `auth::Auth` caches per registry and `store_auth_if_needed`
latches the first), but WP1 ships as a fork PR and the fork has other consumers.

**`RegistryAuth::Basic` has the identical defect and must be excluded on the same grounds.** `_auth()`'s
fallback arm builds `RegistryTokenType::Basic(username, password)` **from the caller's `authentication`
argument** (`client.rs:915-925`), and `auth()` caches it (`:872-880`). So a cache-first return serves the
*previous* caller's credentials: authenticate `repo` with Basic `(old-user, old-pass)`, then call `auth()`
with `(new-user, new-pass)` for the same key, and the next request authenticates as the old identity. The
generalised rule: **the token cache may short-circuit only a credential the *registry* minted (a bearer
token from a realm exchange). Anything the caller handed in is re-derived every call**, because the cache
key carries no credential identity and cannot tell one caller's secret from another's.

**D-001e — the renewal margin must bind the *acquisition* path too, not only `TokenCache::get`.** D-001c
puts the margin in `get` (`token_cache.rs:152-175`), but `auth()` returns the freshly minted token directly
after `tokens.insert()` (`client.rs:865-870`) — it never reads back through `get`. A token endpoint
answering with 10 s of life therefore hands that token to the leader *and*, under D-003, to every coalesced
waiter, none of which consult the margin. A delayed manifest request then sends it after expiry. So the
margin is applied at the point the token is accepted: a newly acquired token whose remaining lifetime is
already inside the margin is not returned as usable — it is re-acquired, or the caller proceeds knowing
C-020's 401 retry is the net. Contract: C-029 gains this half.

**D-001b — the "no challenge required" case is answered by D-003a's host cache, not by a new latch.**
A registry answering `GET /v2/` with `200` and no `WWW-Authenticate` makes `_auth()` return `Ok(None)`
(`client.rs:910-913`), and `auth()`'s `Ok(None)` arm inserts nothing (`:882`) — so a token-cache-only early
return leaves the cache cold and every call still pays one `GET /v2/`. Reaching zero needs the *challenge
probe result* cached, which is exactly D-003a, at **host** granularity where the probe itself lives. It is
**not** a `HashSet` of "anonymous registries": that is the containers/image#2754 shape C-005 forbids, and
what keeps the two apart is D-003a's **purge-on-401** — a later per-repository 401 drops the whole host
entry and the challenge is re-derived. Cache the probe, never the conclusion "this registry needs no auth".

**D-001c — a renewal margin is required, because this change removes an accidental refresh.** Today
`auth()` re-runs `_auth()` and re-`insert`s on **every** `ensure_auth` call — once per registry operation
*and once per layer* on a pull — so the cached entry is continuously replaced by a freshly minted token.
After D-001 it ages to its recorded expiry instead. Expiry is enforced only in `TokenCache::get`
(`token_cache.rs:152-175`) with a strict `epoch > *expiration` comparison, **no skew and no renewal
margin**, against wall-clock `SystemTime::now()` — and `DEFAULT_TOKEN_EXPIRATION_SECS` is 60 s, the Docker
spec's own floor. An entry with sub-second validity left passes that check and the request it authorises
arrives expired; today the accidental refresh masks it, after D-001 it does not. So `TokenCache::get`
treats an entry expiring within a **30 s margin** as a miss. `token_cache.rs` is already in scope, and the
compensating control for whatever slips through is C-020's 401-refresh-and-retry-once — which is why C-020
is not optional and belongs in the same work package (its home is corrected in C-020).

**D-001d — `.expect("Time went backwards")` on this path is replaced.** `token_cache.rs:160`, `:195` and
`:211` panic in library code on a backwards clock step. Pre-existing, but D-001 makes this path newly
load-bearing for every `auth()` call, and a clock step is exactly the condition an expiry cache is exposed
to. A backwards step resolves to "expired" (fail-safe: re-authenticate), never a panic.

The reason the fix must live in the fork and not in `ocx_lib` is forced, not chosen. **Ten** tests in
`crates/ocx_lib/src/oci/client.rs::tests::ensure_auth` assert an *exact* `OciTransport::ensure_auth` call
count (counted from source; the discovery pass reported eight and missed
`pull_description_authenticates_with_pull`):

- Nine assert `assert_eq!(calls.len(), 1)` for a single operation — `list_tags_`,
  `list_repositories_`, `fetch_manifest_digest_`, `fetch_manifest_`, `pull_manifest_`, `pull_blob_`,
  `head_blob_`, `pull_layer_authenticates_with_pull`, `pull_description_`.
- `client_ensure_auth_delegates_to_transport` asserts 1 then 2 across two operations.
- Three more (`push_package_`, `push_description_`, `merge_platform_into_index_`) assert
  `!calls.is_empty()` plus first-call-is-`Push` — order, not count.

Coalescing at the `ocx_lib::oci::Client` level — skipping the transport call when a local check says
"already authed" — drives that count to 0 and breaks all ten. Making the fork's `auth()` cheap on a cache
hit leaves every one of them green.

### D-002 — `store_auth_if_needed`'s side effect is preserved, unconditionally

`crates/ocx_lib/src/oci/client.rs:5250-5253`, verbatim:

> ```
> /// Regression guard for the 401-on-default-mode bug: `pull_layer` must
> /// authenticate before the layer blob fetch. `pull_blob_to_file` sends a
> /// token only if one is already cached, and a cache-resolved manifest
> /// never seeds it, so without this the fetch is anonymous (401).
> ```

`get_auth_token()` reads `self.auth_store.read().await.get(registry)?` first — a `None` there means
anonymous, which is a 401 on a private registry. `ensure_auth`'s *necessary* job is guaranteeing
`store_auth_if_needed` has run once per registry: a cheap `RwLock` write, **zero network**. Not
re-authenticating from scratch.

So the early return in D-001 sits **after** `store_auth_if_needed`, never before it.

### D-003 (A1b) — The token fetch is coalesced per `(registry, repository, operation)`

**The coalescing seam is the token *acquisition*, not `get_auth_token`.** This is the load-bearing
correction: `get_auth_token` (`client.rs:451`) has exactly **one** caller —
`RequestBuilderWrapper::apply_auth` (`client.rs:2731`) — which runs *after* `ensure_auth` has already
warmed the cache. The `index sync` stampede does not route through it at all:

```
LocalIndex::refresh_* → OciIndex → ocx_lib oci::Client::ensure_auth
  → NativeTransport::ensure_auth   (native_transport.rs:276)
  → NativeTransport::authenticate  (native_transport.rs:51-58)
  → fork Client::auth()            (client.rs:856-885)   ← N-wide, cold, every one misses
```

Coalescing only `get_auth_token` would leave the concurrent-cold case unfixed while C-004 passed and
S-006's "concurrent first-contact requests share one exchange" was false — an unchecked green in exactly
the sense `quality-core.md` names.

So the primitive wraps the `_auth()` + `tokens.insert()` pair, keyed on the same
`TokenCacheKey { registry, repository, operation }` the `TokenCache` already uses
(`external/rust-oci-client/src/token_cache.rs:82-87`), and **both** miss paths route through it: `auth()`'s
(after D-001's cache consultation) and `get_auth_token()`'s. A second benefit falls out: `auth()` returns
`Result`, so the leader's error is observable by waiters, which C-004's edge case (a) requires and which
`get_auth_token`'s `Option` signature (`self._auth(...).await.ok()??`, `client.rs:461`) cannot express —
there, a waiter observes `None`, i.e. anonymous, i.e. a later 401.

**The fork cannot use `crate::utility::singleflight`** — separate crate. Injecting a coalescing trait from
ocx the way `dns_resolver` is injected (§10.1's cited precedent, `9c1e5c7`) would be *more* machinery than
the primitive it replaces, so the fork gets its own. **Try a per-key `Arc<OnceCell<RegistryTokenType>>`
stored in the existing `TokenCache` map first**, and fall back to
`Arc<Mutex<HashMap<TokenCacheKey, watch::Receiver<...>>>>` (the containerd `map[key]*authResult` +
`sync.WaitGroup` equivalent) only if expiry-driven replacement gets ugly. §6.5 rejects `OnceCell` **for
ocx_lib**, and both of its reasons are ocx_lib-specific and do not transplant: there are no exit codes
inside the fork (a promoted waiter simply re-runs the token exchange, which is correct for a token fetch,
not a divergence), and the fork's negative result — "no token" — must **not** be cached anyway, per the
containers/image rule below. This is the one place in this plan where a coalescing primitive is written
rather than reused, and the reason is a crate boundary, not a preference.

**D-003a — the challenge probe is cached at host granularity, under the per-scope token cache.**
containerd's `authorizer.go` is two-tier and the fork is one-tier: `dockerAuthorizer.handlers` is keyed
**per host** (the `WWW-Authenticate` challenge is probed once per host for the process lifetime), with
`authHandler.scopedTokens` nested underneath it. The fork's `_auth()` builds its `GET /v2/` probe from
`image.resolve_registry()` alone — no repository component, so the probe is already host-invariant — but
`TokenCacheKey` carries the repository, so **every distinct package's first touch still pays the probe**.
On a P-package sync that is P redundant round trips, and it is why §13's "3 → 1 requests" holds for a
repeated operation on an already-touched repository but **not** for the first touch of each package, which
is where `index sync` spends most of its auth budget. A `HashMap<String /* host */, ChallengeInfo>` guarded
the same way as the token map closes it: 2 → 1 on every package after the first.

**A host-level challenge cache requires containerd's purge-on-401, and it is not optional.** Once the
challenge is cached rather than re-derived per call, a stale challenge can suppress a legitimate later
401 — the coarser-granularity form of the containers/image#2754 class this decision already guards
against. containerd's answer is `invalidAuthorization()`: on a first-attempt 401 whose `WWW-Authenticate`
carries an `error` parameter, delete the **whole host handler** (challenge and every scoped token under
it) so the next request rebuilds from scratch.

**But the trigger is widened here, deliberately, and the reason is a fork-specific gap.** containerd's
narrow `error`-parameter test misses the ordinary case: a registry that has *revoked* a token commonly
answers `401` with a plain `Bearer` challenge and no `error=invalid_token`. Under the narrow rule the host
challenge and its scoped tokens survive, the next operation reuses the revoked token, and the new challenge
is never seen. So the rule is: **the first authenticated `401` for an operation purges the host entry —
challenge and every scoped token under it — and retries once**, which is exactly C-020's single retry. A
*second* consecutive `401` after a fresh token is an authentication failure (exit 80), which is what stops
this from looping and is why the wider trigger is safe.

**This needs a fork-side change to keep the response's headers.** `validate_registry_response`
(`client.rs:2541-2546`) reduces every `UNAUTHORIZED` to `OciDistributionError::UnauthorizedError { url }`
and **discards the response headers**, so nothing downstream can read the `WWW-Authenticate` the retry must
re-derive the challenge from. Preserve the challenge from that 401 for the one retry, in the same change. If D-003a is descoped for
size, it goes to §7 with a reversal trigger, and §13's request-count claim is restated — it does not
silently stay in the summary.

**One credential-bleed guarantee is currently implicit and must be written down.** `TokenCacheKey` carries
no credential identity, so if two different `RegistryAuth` values were ever live for one key concurrently,
a coalesced waiter could receive a token minted for someone else's credentials. It cannot happen today:
`store_auth_if_needed` (`client.rs:424-446`) is **first-write-wins per registry** and never overwrites for
the life of a `Client`, so one `Client` has at most one identity per registry. Record that as an explicit
invariant on `TokenCacheKey` (a doc comment) rather than leaving it as an accidental consequence — a later
change to `store_auth` for credential rotation would silently reopen it and nothing today would catch it.

**Cache key rule, from the field survey.** The key must carry the full scope — repository **and** verb set
— never the registry alone. buildkit's `insufficient_scope` bug class
([moby/buildkit#5883](https://github.com/moby/buildkit/issues/5883)) is what under-scoping looks like in
production. The fork's existing key already satisfies this; the coalescing key must not be widened.

**Do not latch "this registry needs no auth".** containers/image's `docker` transport pings `/v2/`, gets
200, and concludes the whole registry is unauthenticated — then ignores a later per-repository 401
([containers/image#2754](https://github.com/containers/image/issues/2754)). A cached negative must not
suppress a legitimate later challenge from a specific repository.

**Token expiry stays as the fork computes it today** — `parse_expiration_from_jwt` already handles the
non-JWT/opaque case (GHCR) by falling back to `default_expiration_secs` (`token_cache.rs:205-218`). No
change; recorded because the survey's "treat tokens as opaque, trust the endpoint's stated lifetime" advice
is already implemented.

### D-004 (A4) — `fetch_root_document` populates `cache.roots`

`OcxIndex::fetch_root_document` (`ocx_index.rs:1169-1188`) issues `GET p/<ns>/<pkg>.json` and discards
everything but the return value. It writes the parsed root into `self.cache.roots` under the same key
`resolve_root` reads (`repository.to_string()`), so the fan-out that follows hits a warm cache.

This is a **sequencing** bug, not a race (research §2, accepted): the fetch that should have populated the
cache did not. Adding coalescing here would *mask* it — a later waiter would coalesce onto nothing, because
the leader already finished without registering. The fix is the missing insert.

`resolve_root` memoizes misses as well as hits (`Option<Arc<IndexRoot>>`), and `fetch_root_document`
returns `Ok(None)` on a confirmed 404 — so the negative must be cached too, on exactly the same terms:
**only a confirmed 404.** Any other status is `IndexHttpFailed` and caches nothing (this is what
`jurisdiction`'s `Outside` verdict is settled off; see `ocx_index.rs:796-804`).

**D-004a — there is a *third* `Ok(None)`, it issues no request, and inserting on it corrupts reads.**
`fetch_root_document` opens with `if !self.serves_registry(identifier.registry()) { return Ok(None); }`
(`ocx_index.rs:1174-1176`) — a foreign-registry identifier, answered without contacting anything. Meanwhile
`resolve_root` memoizes under `repository.to_string()` **with no registry component** (`:809`, `:820-824`).
So the obvious implementation — a tail-position
`cache.roots.insert(repository.to_string(), root.clone())` — poisons key `ns/pkg` with `None` when called
for `ghcr.io/ns/pkg`, and the next `ocx.sh/ns/pkg` resolve reads that memoized `None`, `jurisdiction`
settles `Outside`, and the package silently stops resolving through the index **for the rest of the
process**. That is a local-first read-semantics change of exactly the class C-027 exists to forbid, and the
foreign-identifier call shape is supported and exercised
(`ocx_index.rs::tests::fetch_root_document_returns_none_for_foreign_namespace` at `:2737`;
`package/cascade/gather.rs:151-160` calls through `Index::from_source`).

**The insert happens only on the two paths that issued the request** — `IndexFetch::Found` and a confirmed
`IndexFetch::NotFound`. The `serves_registry` early return memoizes nothing. C-006 edge case (a) gains a
companion asserting exactly that.

`fetch_root_document` runs `check_format_version()` first, exactly as `resolve_root` does. That ordering is
unchanged.

### D-005 (A8) — Singleflight on `OcxIndex::{check_format_version, resolve_root}` and `OciIndex`'s tag caches

The genuine-race half. Under a 64-wide fan-out, N tasks read-check the same key, all miss, all fetch.
`utility::singleflight::Group` is the answer — the repo's own primitive, already proven in three call
sites, and the only surveyed option satisfying **both** hard constraints:

- **Negative results must be cached and broadcast.** A confirmed 404 is a positive, load-bearing result
  (it settles `jurisdiction`). `moka::try_get_with` is built around the opposite default — a failed init
  leaves the key absent for the next caller to retry.
- **Exit-code parity.** `Handle::drop` broadcasts `Error::Abandoned` to every waiter, so all callers of one
  logical operation get one outcome. `tokio::sync::OnceCell` silently promotes a waiter to retry on leader
  cancellation, so two callers of the same operation can walk away with different exit codes.

**D-005a — `Group` retains failures for the group's lifetime, and that must be fixed in the primitive
before it can be used here.** Verified in source: `try_acquire`'s map-hit arm is
`Some(Err(e)) => return Err(e)` (`singleflight.rs:210-214`), unconditional and with no expiry; the doc says
"Resolved entries are retained for the group's lifetime" (`:155-158`) and offers *scoping*, not eviction,
as the mitigation; there is **no** eviction API (`new` and `try_acquire` are the only methods, and the file
contains zero `remove`); and `Handle::drop` broadcasts `Error::Abandoned` (`:139-145`), which is retained on
the same terms — so a leader that is merely **cancelled** poisons a key that was never attempted to
completion. Poisoned entries also consume `max_entries` forever, so after enough distinct failures the group
answers `CapacityExceeded → TempFail(75)` to every unrelated call — an exit code advertising "transient"
for a condition no retry inside that process can clear.

Against a **process-lifetime** group that is not a lost optimisation, it is a namespace outage: D-004 says
"any other status is `IndexHttpFailed` and **caches nothing**", C-006(b) says a non-404 failure re-requests,
and `subsystem-oci.md`'s jurisdiction rule is deliberately fail-closed — a root fetch that errors keeps the
source `Authoritative`, and `Authoritative` is a terminal stop. Under retention one 503 blip pins that
source authoritative for a name it can now never answer, with no fall-through and no way to clear it.
`check_format_version`'s own doc promises a transport failure "propagates as a hard error on **every
call**", which retention converts into *propagates the first error forever*.

**Decision: eviction-on-read in the shared primitive.** `try_acquire`'s `Some(Err(_))` arm drops the entry
and hands the asking caller fresh leadership. Removal on the **read** side, not the write side, is the
load-bearing detail: a waiter that already entered `wait_for` holds its own `watch::Receiver` clone and
never re-consults the map, so it still receives the leader's `Failed`/`Abandoned` verbatim — the exit-code
parity property this decision chose `Group` for survives untouched, while the property nobody chose (that
callers of a *later, different* operation inherit it) goes away. `Handle` and `Drop` are unchanged, no
signature changes, no new bound, and replacing in place leaves `entries.len()` unchanged so a failed key
never holds a capacity slot hostage.

**This is not a new defect introduced by D-005 — it is present in the tree today.** `ChainedIndex`'s group
(`chained_index.rs:200`, built once at `context.rs:292`, shared across `box_clone` at `:1481`) is already
process-lifetime, and `project::resolve::resolve_work`'s `set.abort_all()` (`project/resolve.rs:138`) fires
the abandonment path on it. No user-visible failure follows today only because that CLI exits immediately;
the moment a group outlives one failure the poison is permanent. Two existing tests state the old decision
and are rewritten with the change, not deleted around it:
`subsequent_acquire_after_failure_returns_error` (`:406-419`) becomes a fresh-leader assertion, and
`failed_error_is_durable_across_multiple_acquires` (`:437-452`) is superseded.
`failed_leader_propagates_error_to_waiters` (`:385-404`) and `abandoned_handle_signals_error` (`:341-357`)
**must stay green unchanged** — they are the guard that eviction did not break in-flight broadcast.
Full evidence, consumer audit and measured mutation results:
[`decision_singleflight_error_eviction.md`](./decision_singleflight_error_eviction.md).

**D-005b — two things eviction does *not* fix, and D-005 needs both.**

1. **C-007's assumed-v1 arm must bypass the group.** `check_format_version` deliberately does not memoize
   the 404 → `assumed_v1()` result (`ocx_index.rs:783-789`) so a tree that later publishes a `config.json`
   is picked up without restarting the process. That value is `Ok`, so eviction-on-failure leaves it
   memoized. The group covers the served-document case only.
2. **`max_entries` must be sized for the run, not copied.** `SINGLEFLIGHT_MAX_KEYS = 1024` was chosen for
   per-identifier refresh in `chained_index.rs`. Copied into a process-lifetime `OcxIndex` group, an
   `ocx index sync` against a registry holding more than 1024 packages hits `CapacityExceeded` →
   `TempFail(75)` on **successes** alone.

Precedent to mirror for the group's *shape*: `chained_index.rs`'s
`Group<String, Option<(Digest, Manifest)>>` — but see D-005b(2) on the constant.

**Sharing rule, carried from `chained_index.rs` verbatim.** That group's key does not encode write policy,
so `read_only()`/`remote_view()` build a **fresh** group rather than sharing the parent's — "sharing the
parent's group could coalesce a read-only resolve onto a persisting leader". `OcxIndex`'s groups carry
no write policy at all (nothing in `OcxIndex` writes locally), so they share across `box_clone` like the
existing `SourceCacheInner`. This is stated so a future reader does not copy the wrong half of the
precedent.

`OciIndex`'s `Cache` (`oci_index/cache.rs`) gets the same treatment for `tags` and `tag_digests`. Its
deliberate no-outer-lock design (`SharedCache = Arc<Cache>`, each field its own `RwLock`) is preserved — a
`Group` is added beside the maps, not around them.

### D-006 (A3) — The published dispatch fetch is gated on `read_dispatch_object`

`persist_dispatch` (`local_index.rs:378-398`) calls `fetch_manifest_raw_bytes` unconditionally. A gate is
added on the content-addressed store — **in `refresh_published`, immediately before its `persist_dispatch`
call (`local_index.rs:262`), and not inside `persist_dispatch` itself.** `persist_dispatch` is unchanged
by this item.

**The placement is load-bearing, for two independent reasons.** `persist_dispatch` is `pub` with four
production callers — `refresh_published` (`local_index.rs:262`), `refresh_derived` (`:313`), and **two on
the ordinary resolve path**: `ChainedIndex` dispatch resolution (`chained_index.rs:727`) and
`ChainedIndex::fetch_and_persist_chain` (`:1675`). First, the gate is not computable there: the function
receives a *tagged* identifier and learns the digest only from the fetch it would be skipping, whereas
`refresh_published` already holds `root.tags[tag].content`. Second, `persist_dispatch` returns
`Option<(Vec<u8>, Digest, Manifest)>` and both resolve callers consume all three, so a skip inside it has
nothing to return without re-reading and re-parsing — turning a fetch-skip into a behaviour change on the
hottest path in the binary, under `LocalWritePolicy::Full`, across the `ChainMode`/`LocalWritePolicy`
boundary §11 asserts is untouched.

The gate, wherever a caller holds the digest in advance:

| `read_dispatch_object(source, repo, digest)` | Action | Why |
|---|---|---|
| `Ok(Some(_))` | **skip the fetch** | present, and hash-verified against the digest |
| `Ok(None)` | fetch | absent |
| `Err(DigestMismatch)` | **fetch** | present but corrupt — this is the self-heal opportunity |

**`Path::exists()` is forbidden here, and the reason is a permanent-corruption bug, not style.** From
`index_store.rs`'s own design constraint: *"There is no `has_dispatch_object`/exists-only method, and none
should be added — bare `path.exists()` is unsafe here. Self-heal only happens on the WRITE path (needs the
correct bytes in hand); a bare existence check cannot distinguish a valid object from a zero-byte crash
artifact or tampered file, and if a caller treats 'exists' as 'already have it' and skips the fetch, a
corrupt object is permanently stranded."*

**Verified: there is no independent backstop.** All three candidates were checked against source:
- `AbsentDispatch` recovery fires on *absence*; a `DigestMismatch` does not route into it.
- `ocx index regenerate` rebuilds `c/index.json` from the `p/` walk and never touches `o/` bytes.
- `ocx package verify` is Sigstore signature verification, unrelated to index CAS integrity.

So collapsing `Err(DigestMismatch)` into "skip" (via `.ok()`, or `if let Ok(Some(_))`) or letting it
propagate as fatal without fetching **strands the corruption forever**. This is the single line a reviewer
must check on this item.

**Cost model — the local hash is strictly cheaper than what it replaces.** The alternative to hashing
locally is fetching the object over the network *and then hashing it anyway*: `write_verified_object`
(`index_store.rs:156-164`) recomputes `sha256(bytes)` on every write as a CWE-345 trust-boundary check.

| | Fetch path (today) | Gated path (D-006) |
|---|---|---|
| Network | full object transfer | none |
| Hash | **twice** — the fetched bytes in `write_verified_object`, then the on-disk bytes in its own existing-file short-circuit (`index_store.rs:382-385`) | **once** (`read_dispatch_object`) |

The gated path therefore does *less* hashing than today, not more — `write_dispatch_object` already
short-circuits on an existing correctly-hashing file, so the unchanged-object case currently pays a network
transfer **and** two hashes to reach the same conclusion the gate reaches with one hash and no network.

`MAX_INDEX_DOCUMENT_BYTES = 32 MiB` is a DoS bound, not an expected size — a real image index for a handful
of platforms is single-digit KB. The "re-hashing per check cancels the optimization" argument is therefore
rejected (§6.4), and so is the periodic-scrub subsystem it implies: the gated path already self-heals,
because `Err(DigestMismatch)` routes to fetch and the fetch's `write_dispatch_object` overwrites.

**Published-path payoff is the whole round trip, and this is why A3 and A12 are separate items.**
`refresh_published` already holds `root.tags[tag].content` from `fetch_root_document` — the root document
*is* the tag→digest answer, already on the wire. The CAS check costs **zero** extra requests. Per package
on an unchanged re-sync: 1 root GET, 0 dispatch GETs.

### D-007 (A12) — The derived path does HEAD-then-skip

**Status: descoped.** Both of D-007a's triggers below fired — a leaf-heavy tag population is a permanent
regression, and a `Docker-Content-Digest`-omitting registry pays a double body fetch — so A12 was built,
measured and reverted rather than shipped. Retained as the design rationale and rejected-option record;
see `decisions_index_sync_perf_autonomous.md` §E for the measured request counts.

A derived source has no root document, so the digest must be asked for. `fetch_manifest_digest`
(`oci_index.rs:74-91`) is a HEAD-shaped lookup that is **already cached** in `OciIndex::Cache`. So:

1. `fetch_manifest_digest(tag)` → digest `D` (in-process cached; coalesced by D-005).
2. `read_dispatch_object(D)`:
   - `Ok(Some(_))` ⇒ record `(tag, D)` and **skip the body GET**.
   - `Ok(None)` ⇒ fetch the body.
   - `Err(DigestMismatch)` ⇒ fetch the body (self-heal).

**`Ok(Some(bytes))` is decoded, not assumed.** `records_root_tag` (`local_index.rs:1128-1138`) returns
`false` unless the manifest is an `ImageIndex`, and its doc gives the reason — recording a bare-manifest
tag *"would create exactly the tag-without-an-object absence D1 abolished"*. Today `refresh_derived` gets
that manifest from `persist_dispatch`'s fetch (`:313-315`); removing the body removes the manifest, and the
tempting inference — *only image indices are ever written to `o/` (D1/D2), so presence at `D` proves the
shape* — **trusts the tree's own write history, which is not the only way `o/` gets populated**. The local
tree is a distributable artifact (`adr_index_indirection.md` A2: a devcontainer feature, a CI artifact, a
committed `.ocx/`) and `subsystem-oci.md` calls it *"trusted deployment-managed input"*. A hash-correct
but wrong-shape object in a copied tree is exactly what `decode_index_manifest` fails closed on today
(*"a leaf platform manifest, a truncated file, or any other payload is refused here … never a silent load
of the wrong shape"*, `:1148-1156`) and what the inference would record as a version.

The check costs **zero** extra work: `read_dispatch_object` already returns the bytes it just hashed
(`index_store.rs:415-425`), so `decode_index_manifest(&bytes)` is one call on data in hand. So step 2 is:
`Ok(Some(bytes))` ⇒ decode; `Some(index)` ⇒ record and skip the body GET; `None` ⇒ **fetch**, same as
`Err(DigestMismatch)`.

**This is A12's problem alone. D-006 / the published path is unaffected** — `refresh_published` commits the
fetched root document's bytes verbatim through `commit_published_root` (`local_index.rs:269`) and never
calls `records_root_tag`; the published root is the index operator's authored document.

**`Ok(None)` is genuinely ambiguous and must fetch.** A single-platform tag writes nothing to `o/`
(`persist_dispatch`'s `Manifest::Image` arm), so "absent from `o/`" means either "never fetched" or "it is a
leaf". A12 therefore saves the **body**, not the round trip — one HEAD replaces one GET for an
already-held multi-platform tag. **And a leaf tag is not merely "refetched exactly as today": it costs one
request *more* than today, permanently.** Today `refresh_derived` issues exactly one request per tag
(`persist_dispatch` → `fetch_manifest_raw_bytes`, `local_index.rs:313`). After D-007 a single-platform tag
costs `fetch_manifest_digest` (a HEAD) **plus** the same GET, every run, because a leaf never has an `o/`
object for the gate to hit. Against a registry whose tags are predominantly single-platform, A12 makes the
command **slower**. This is recorded as an accepted negative in §13 and is a second trigger on D-007a's
descope: if the measured tag population is mostly leaves, A12 is dropped rather than shipped. Stated
plainly because the honest payoff is much smaller than A3's and, for one real population, negative.

**D-007a — On a registry that omits `Docker-Content-Digest`, A12 is a net loss, and it is gated on that.**
The fork's `fetch_manifest_digest` (`external/rust-oci-client/src/client.rs:991-1031`) is HEAD-first with a
documented fallback: *"Will first attempt to read the `Docker-Content-Digest` header using a HEAD request.
If this header is not present, will make a second GET request and return the SHA256 of the response body."*
Against such a registry, A12 costs HEAD + GET where today it costs one GET.

The header is **required** by the OCI distribution spec on pull responses, so the fallback is a
non-conformance path — but non-conforming registries exist, and a "performance" item that is a pessimisation
against one of them is not acceptable silently. Two mitigations, both cheap:

1. The digest lookup is **memoized in `OciIndex::Cache::tag_digests`** and coalesced by D-005, so the extra
   round trip is paid once per `(identifier)` per process rather than per lookup.
2. The fallback GET returns the body the caller was going to fetch anyway. **The implementation must
   surface it rather than discard it** — otherwise the fallback path fetches the same manifest twice. This
   is the same fetch-discard-refetch shape as [#314](https://github.com/ocx-sh/ocx/issues/314) and
   [#319](https://github.com/ocx-sh/ocx/issues/319), and re-introducing it inside a fix for it would be a
   poor outcome. If surfacing the body needs a fork-side signature change, **A12 is descoped rather than
   shipped with the double fetch** — its payoff does not justify a second fork change.

### D-008 (A7) — A package's completed tags survive a sibling tag's failure

The two `try_collect` sites become `collect::<Vec<_>>()`, and the commit adopts only the tags that
succeeded.

**Three sub-decisions, because the naive change is wrong.**

**D-008a — The failure unit is the content digest, not the tag.** `refresh_published` dedups tags by
`entry.content` before fanning out (`local_index.rs:238-248`) — one representative tag per distinct digest.
So a failure for representative tag `T` means every tag aliasing the same digest is equally unpersisted.
The adopted set is computed by digest: adopt tag `t` iff `root.tags[t].content` is **not** in the failed-
digest set.

**D-008b — `RootScope::Package` cannot be used for a partial commit.** `merge_root`'s
`RootScope::Package` arm (`local_index.rs:1050-1056`) adopts **every** tag the fetched root lists, with no
filter. Committing a partial refresh under that scope would pin tags whose dispatch object was never
written — exactly the tag-without-an-object state D1 abolished.

Therefore `RootScope` gains the ability to name a tag set. Preferred shape, chosen for type economy over
adding a third variant:

```
RootScope::Tags(&'a [&'a str])   // replaces Tag(&'a str); Tag(t) becomes Tags(&[t])
RootScope::Package               // unchanged — full success only
```

One variant covers the named-tag write, every grow-on-resolve, and the partial-success write. The cost is
a slice at the existing `RootScope::Tag` call sites (`refresh_published`, `fetch_and_persist_chain`, and
the `merge_root` tests).

**D-008c — Package-level fields are adopted only on full success.** Under `RootScope::Package`,
`merge_root` also adopts `repository` and every human-governed package-level field. `repository` is the
*other* half of what the local copy pins (`adr_index_indirection.md:1037-1039` — "the copy pins both halves
… tag → digest, and logical → physical routing"). Taking a routing migration while some of the package's
tags were not refreshed would leave unrefreshed pins routing through a repository they were never observed
at. So: **all tags succeeded ⇒ `RootScope::Package` (routing adopted); any tag failed ⇒ `RootScope::Tags`
over the succeeded set (routing untouched, retried next run).** This is the conservative half of an
otherwise-relaxed rule, and it is chosen because the failure mode of the other choice is silent and the
failure mode of this one is one extra sync.

**D-008d — Two guards constrain the implementation, and only one of them can see the file being edited.**

**Read this before citing either guard.** `index_common.rs`'s two guards live in `ocx_cli` and cannot
observe `ocx_lib`: `no_index_module_outside_this_one_grows_a_refresh_fan_out` roots its scan at
`crates/ocx_cli/src/command/`, and `the_funnel_neutralizes_both_halves` reads
`include_str!("index_common.rs")` — one file. D-008 edits
`crates/ocx_lib/src/oci/index/local_index.rs`. **Neither `index_common.rs` guard constrains it.**

- **It must stay inside `buffer_unordered`.** The guard that *does* cover the file is
  `local_index.rs::the_per_tag_fan_out_is_sized_by_the_constant_at_every_site` (`:1279-1300`), which pins
  `buffer_unordered(` at exactly 2 and `buffer_unordered(TAG_REFRESH_CONCURRENCY)` at exactly 2. Switching
  `try_collect` → `collect` keeps both counts. **Its strength is limited and the limit is stated here:** a
  `JoinSet` added *alongside* the two surviving calls leaves both counts at 2 and passes. The denylist that
  would catch it exists only in the `ocx_cli` guard. If the "no `JoinSet` in this fan-out" property is to be
  enforced on `local_index.rs`, extend that test with the same denylist — it already builds the
  comment-stripped production window it would scan.
- **It must not add a `log::warn!`.** Here too the pin is in the wrong crate; **C-026 is the contract that
  actually covers `local_index.rs` and `ocx_index.rs`.** For the record,
  `index_common.rs::the_funnel_neutralizes_both_halves` pins
  `log::warn!` at exactly **2** and checks each site's sanitizer argument individually — the design
  principle is per-site verification, so a third warn needs its own assertion block, never a count bump.
  A per-tag failure is already reported by the existing funnel via the package-level error; **D-008 adds no
  new operator-facing log line.** Per-tag detail stays at `log::debug!`, matching `refresh_published`'s
  existing "per-tag detail is debug-only so an index update over a many-tagged package does not flood info
  logs".

**D-008f — An empty succeeded set must never reach `commit_published_root`.** `merge_root`'s tail is
`(changed || !usable).then(|| serialize_root(&root))` (`local_index.rs:1106`), where `usable` is a *typed*
parse of the committed bytes (`:1062`). For a **first-sight package** there is no committed root, so
`usable == false` and it writes **unconditionally** — from the fetched document with `tags` emptied but
every package-level field, `repository` included, adopted (`:1069-1073`). So calling
`commit_published_root(…, RootScope::Tags(&[]))` — the natural shape of "adopt what succeeded" — on a
package whose every tag failed lands a root with `repository` set, **zero** tags and a `c/index.json`
entry. The package then appears in `ocx index catalog` pinning nothing, and D-008c's "any tag failed ⇒
routing untouched" is violated with no tag refreshed at all. The same branch fires when a committed root
exists but does not parse, and there it additionally **drops every previously committed tag** — where
today's `try_collect` leaves the corrupt root intact for the next full success to repair.
`refresh_published` therefore returns the failure **before** reaching `commit_published_root` when the
succeeded set is empty: an explicit guard, not a reliance on `merge_root`. C-012's edge case (a) must use a
**first-sight** package as its fixture — against a package that already has a committed root it is green
for the wrong reason.

**D-008g — `refresh_derived` must surface its failure before the `is_empty` gate, or it silently
reclassifies 69 → 79.** Today `.try_collect()` (`local_index.rs:326`) propagates `IndexHttpFailed` →
`ExitCode::Unavailable` (69) *before* the `if fetched.is_empty()` gate at `:329` raises `NoIndexableTag` →
`ExitCode::NotFound` (79). Switching to `collect` naively lets the empty case reach `:329` first, so a
package whose tag manifests all fail transiently reports **"no indexable tag" at 79** — package absent —
instead of 69, and the actual transport error never reaches the operator. C-024 pins 69 at the *error*
level and cannot see this, because the error is discarded before classification. The same masking applies
whenever the surviving tags all fail `records_root_tag`. Note that every partial-success contract in §5
(C-012, C-013, C-014) is written against `refresh_published`; the derived half needs its own, asserting the
**exit code**.

**C-032 — The derived half's partial success preserves the failure's classification.**
Given a derived source where every tag's manifest fetch fails with a transient transport error, the command
exits **69 (`Unavailable`)** and the surfaced error is the transport failure — not `NoIndexableTag` (79).
*Edge case:* where some tags succeed and the rest fail transiently, the succeeded tags are adopted and the
returned error is still the lowest-index transport failure.
*Red-reachability:* order the `is_empty` gate before the failure check and this must go red on the exit
code; asserting only the message cannot see it.

**D-008e — The returned error is deterministic.** `buffer_unordered` completes out of order, so the
per-tag results are collected as `(input_index, Result)` and the **lowest-index** failure is returned —
the same rule `index_common::first_failure` states one layer up, applied here rather than imported (the
helper is `pub(super)` in `ocx_cli`; a three-line `min_by_key` in `ocx_lib` is the smaller change than
promoting it).

### D-009 (A11) — `index_catalog.rs --tags` gets a bounded fan-out

`index_catalog.rs:50-71` spawns one `JoinSet` task per repository with no cap. It is exempt from the
snapshot-spec C-024 guard **because it is read-only** (it never calls `refresh_tags`), not because it is bounded — the
exemption comment says so.

It gets a `tokio::sync::Semaphore`-bounded fan-out. **`buffer_unordered` is rejected here**, though it is
the shape `index_common.rs` uses: `JoinSet` surfaces a panicking task as `JoinError::is_panic()` at the
join boundary, and `index_catalog.rs:97` depends on that — it aborts the remaining tasks and
`resume_unwind`s, "matching the `index update` JoinSet panic precedent". `buffer_unordered` has no task
boundary, so a panic unwinds the caller directly and S-008's "a task panic still aborts the rest and
propagates" would have to be restated. Keeping the `JoinSet` and gating `spawn` on a permit preserves
S-008 verbatim and is the smaller diff. **The permit must not reuse `INDEX_REFRESH_CONCURRENCY` as a
permit *class*.** Both #316 and #167 independently record the rule: an
inner fan-out reusing the *same* permit class deadlocks (an ancestor holds a permit while waiting on
children that cannot acquire one); a **separate permit class is deadlock-safe**. `index_catalog` is a
top-level loop with no ancestor holding a refresh permit, so either is technically safe here — but a
separate constant is used anyway, so the rule holds by construction rather than by an argument about the
current call graph.

The guard's exemption list must be updated in the same commit if the module stops matching:
`no_index_module_outside_this_one_grows_a_refresh_fan_out` asserts `seen_exempt.len() == exempt.len()`, so
removing `JoinSet` while leaving the name in `exempt` is fine, but renaming the file fails the guard.

### D-010 (A5) — One retry ladder, on the index-document path

A retry ladder is added at the **transport** seam — inside `ReqwestIndexTransport::get`, where the
`reqwest::Response` and its status are in hand — not at the `OcxIndex` method level.

**It covers both of `get`'s phases, and this must be said because no status-based contract can tell.**
`get` is `send()` (`ocx_index.rs:220-228`, status in hand) *plus* a chunked body loop (`:266-285`, where the
only error is a transport error carrying no status). Every retry contract below fails in the **first**
phase — C-016 on statuses, C-017 on `Retry-After` headers, C-018 on delays, C-019 on 503-per-request — and
C-021/C-028 assert only final outcome and the outer cap. **None of them observes a retry after headers have
arrived.** So a ladder written as a loop around `send()` — the literal reading of the sentence above —
leaves the body unretried and passes every contract, while failing on exactly the shape #330 reports: a
TLS-inspecting proxy that accepts, returns `200` plus headers, then resets mid-body. C-016 carries the
discriminating case.

**Shape**, adapted from oras-go's field-tested defaults rather than derived from scratch:

| Property | Value | Source |
|---|---|---|
| Retryable statuses | `408`, `429`, `500`, `502`, `503`, `504` | oras-go default; GCS documented default; ACR's own guidance for its 429s |
| Retryable transport errors | connect failure, timeout, connection reset/close | GCS documented set |
| Backoff | full jitter: `sleep = random(0, min(cap, base * 2^attempt))` | AWS Architecture Blog — measured to do less total work than equal jitter under contention |
| `base` | 250 ms | round figure near oras-go's own default — oras-go's `MinWait` is **200 ms** (`registry/remote/retry/policy.go`), not 250 ms as this table previously claimed |
| `cap` | 3 s | oras-go |
| Attempts | 3 (initial + 2 retries) | Google SRE Book's per-request floor |
| `Retry-After` | **honoured when present**, on `429` and `503`, **clamped to 30 s** | RFC 9110 §10.2.3 — parse **both** delay-seconds and HTTP-date forms; ACR documents the value counts down across polls, so never cache the first value seen |

**The clamp is a security control, not a tuning knob (CWE-400).** A `Retry-After` is attacker- or
misconfiguration-controlled input from a header. `Retry-After: 86400`, or an HTTP-date years out, would
otherwise freeze `ocx index sync` for that duration on a single header — one CDN edge, one header, every
`ocx` in an unattended CI fleet stalled. Rules, all three testable:

- A value **above the clamp means stop retrying now**, not sleep that long: the request fails with the
  status intact and the caller sees a prompt, honest failure.
- A **past-dated** HTTP-date resolves to zero, never to a computed negative or a wrapped duration.
- An **unparseable** value resolves to zero and the normal jittered backoff applies.

**Real-world corroboration, found after the clamp was designed, not the reason for it.**
[docker/hub-feedback#2459](https://github.com/docker/hub-feedback/issues/2459) documents Docker Hub
sending a raw Unix timestamp (`1746136938`) in its proprietary `X-Retry-After` header on a rate-limit
response — not RFC 9110's standard `Retry-After`, so this is corroboration of the input shape, not a
claim that the standard header carries it. Traced through this code, that value parses as delay-seconds
(`parse_retry_after`, `crates/ocx_lib/src/oci/transport_policy.rs:105`) into
`Duration::from_secs(1_746_136_938)` — a ~55-year sleep — which exceeds `retry_after_clamp` and returns
`RetryDelay::StopRetrying` (`:176`) instead of honouring it. Exactly the CWE-400 shape the clamp exists
to stop, observed in the wild rather than hypothesised.
| Never retried | `401`, `403`, `404`, and every other 4xx | GCS/AWS guidance; a `404` here is a load-bearing confirmed absence (§D-004) |

**Two rules every source agreed on, both load-bearing at this concurrency:**

1. **Jitter is not optional.** Without it, many of up to 512 in-flight requests hitting one shared
   rate-limit retry in lockstep and reproduce the spike. oras-go bakes 10% jitter in at far lower expected
   concurrency than OCX's ceiling.
2. **The retry budget is global to the run, not per-request.** Google's SRE book records the amplification
   arithmetic (3 layers × 4 attempts = 64×) and the mitigation: track the retry:total traffic ratio and
   stop retrying past ~10%. For a single process fanning out 512-wide against one bottleneck (the proxy),
   the amplification is *within* one process. **A per-run retry budget is part of this decision, not a
   follow-up** — an uncapped per-request policy applied independently 512 times is the failure mode, not
   the fix.

   **Its shape is decided here, because "a budget" with no number, no scope and no owner is not testable**
   (it was the one contract in §5 that failed this ADR's own testability bar):

   | Property | Decision | Why |
   |---|---|---|
   | Mechanism | **ratio**, not an absolute count: a retry is admitted only while `retries / total ≤ 0.1` | SRE's actual mechanism. A constant sized for 200 packages starves a 5,000-package sync and is too loose for a 20-package one |
   | Floor | the first **10** retries of a run are always admitted | without it the ratio makes the first request unretryable (`1/1 > 0.1`), which would delete the ladder for small runs — SRE's client budget carries the same floor |
   | Scope of "the run" | **per `ReqwestIndexTransport`**, which is one per source per process | the transport is the seam the ladder lives at; a process-global counter would couple two sources' failures, a per-request one is the failure mode above |
   | Owner | two `AtomicU64`s (`total`, `retries`) on the transport, incremented with `Relaxed` | a 512-wide fan-out shares it; exactness is not required, monotonicity is. **This makes the budget state, not a value object** — it does not live on `RetryPolicy` |

   **Its jurisdiction is the index-document path only, and that must be said out loud.** The ladder lives
   in `ReqwestIndexTransport::get`; the registry-derived half of a sync issues its requests through
   `ocx_lib::oci::Client` and the fork, under **no** budget. Layering C-020's 401-refresh on top adds token
   exchanges — potentially one per in-flight tag — with no global cap, and ACR meters token exchange in a
   rate-limit bucket *separate* from data pulls, so the unbudgeted class is the one that trips a separate
   limiter whose 429s are then answered by more token exchanges. **This ADR does not extend the budget to
   the registry path**; that half's amplification is bounded by D-003's coalescing (one exchange per
   `(registry, repository, operation)` per process, not one per request) and by C-020 being a *single*
   retry on a distinct budget. Recorded as a decision so §5.5's SRE-book argument is not read as covering
   both halves. If the registry path ever grows its own ladder, it needs its own budget in the same change.

   Contract: C-019.

**D-010a — `IndexHttpFailed` must carry the status structurally.** Today the non-success arm formats it
into a string: `source: format!("unexpected status {status}").into()` (`ocx_index.rs:240-246`). A retry
classifier cannot read a status out of a `Box<dyn Error>` message, and neither can exit-code
classification. The retry lives *inside* `get`, where the status is still typed, so this is not blocking —
but the variant should gain the status as a field so the decision is inspectable and testable. Recorded as
a required sub-change, not an optional cleanup.

**D-010b — A 401 mid-batch is "refresh the token and retry once", not a failure and not a generic retry.**
Per the Docker token spec, `expires_in` defaults to **60 seconds** when omitted and a token "should never
be returned with less than 60 seconds to live". A sync batch behind a slow proxy can outlive that easily,
so tokens acquired at the start can expire for requests still queued. This is a distinct class from
transport/5xx retries and must not share their budget. (The index-document path is static-file HTTP and
usually unauthenticated; this rule binds the registry-derived half of the sync.)

**D-010c — Idempotency is what makes this safe, and it is checked.** Every request on the index path and
every read on the derived path is a `GET` or `HEAD`. No retry is added to any write path in this plan.

### D-011 (A6) — Timeout semantics are inverted to match the failing environment

`build_index_http_client` (`ocx_index.rs:177-201`) drops the hard `.timeout(INDEX_REQUEST_TIMEOUT)` in
favour of the registry client's shape:

| Bound | Today (index) | After | reqwest call |
|---|---|---|---|
| Connect | `connect_timeout(30s)` | `connect_timeout(30s)` — unchanged | `ClientBuilder::connect_timeout` |
| Body | `.timeout(60s)` — hard total-request deadline | **30 s per-frame idle bound** | `ClientBuilder::read_timeout` |
| Outer cap | — | **300 s (5 min) per attempt**, kept as a backstop | `ClientBuilder::timeout` |
| Redirects | `Policy::none()` | `Policy::none()` — unchanged (see D-011a) | — |

**The backstop is per-attempt, not per-retry-ladder, and that is a deliberate choice.** `.timeout()` and
`.read_timeout()` are independent composable builder calls — both apply simultaneously — so keeping
`.timeout()` at a generous value is curl's own recommended composition (fast-fail connect + stall detector
+ generous outer cap) with two calls and no wrapper. A `tokio::time::timeout` around the whole ladder was
considered and **rejected**: total retry *volume* is already bounded by C-019's budget, and a wall-clock
cap on the ladder would abort a run that is making honest progress across several slow-but-not-stalled
attempts — the exact case D-011 exists to stop aborting.

**Worst-case wall time grows, and that is the trade being made.** Today's hard 60 s cap is a de-facto
whole-run bound: a document either arrives in 60 s or the run fails. After D-010 + D-011 the worst case per
document is 3 attempts × 300 s plus backoff ≈ 15 minutes, and the run is **not** bounded in wall-clock by
design — only in retry *volume* (C-019's ratio) and per-attempt duration. This is deliberate: #330's
complaint is that the run **fails**, and the fix for "fails too eagerly" necessarily costs patience. The
mitigations that make it acceptable: the 3-attempt path requires three separate near-cap-length attempts
each ending in a *retryable* status (a slow-but-progressing body is one attempt, not three), the ratio
budget caps retries across the whole run, and a CI caller already carries a job timeout, which is the right
place for a wall-clock bound on an operator-initiated sync. Recorded in §13's negatives, not buried here.

**All three bounds are injectable, carried by the `TransportHardening` value object.** Not for production
configurability — the values above are the only ones shipped — but because C-021's fixture otherwise costs
90 s of real wall-clock per run against real sockets, which `tokio::time::pause()` cannot compress. With
the idle bound as a parameter the fixture asserts the same semantics at a few hundred milliseconds.

Rationale is curl's stated best practice — *"a fast-fail connect timeout, a generous overall cap, and a
speed/stall timeout in the middle"* — and Go's `net/http` caveat that `Client.Timeout` "includes the time
spent reading the response body" and is the wrong tool for a transfer that is merely slow.

The hard 60s cap is not simply raised, because raising it trades the slowloris protection the comment cites
(`ocx_index.rs:95-99`) for the throttled-link case. An idle bound gives both: a genuinely stalled
connection still fires, an honest slow body does not.

**D-011b — the third arm of `build_index_http_client` currently escapes every gate, and it is deleted.**
`ocx_index.rs:177-201` applies `harden` (connect timeout, total timeout, `Policy::none()`) to the seeded
build and to the first fallback, then falls back a second time to a bare `reqwest::Client::new()` — which
carries reqwest's defaults: **no timeouts and redirects followed up to 10 hops**. The function's own
comment claims *"the fallback keeps the same timeout + no-redirect hardening"*, which is true of the arm
above it and false of that one. That client is unguarded by `GuardedResolver`, so a followed 3xx is
remote-controlled egress (CWE-918) and can relocate the fetch to `http://` **after** `resolve_base_url`'s
plain-HTTP gate already ran (CWE-319) — precisely what the comment says `Policy::none()` exists to prevent.

The arm is removed, not hardened: `harden(reqwest::Client::builder()).build()` failing and
`reqwest::Client::new()` succeeding is not a reachable difference — `Client::new()` is itself
`builder().build().expect(...)` — so the third arm trades a panic for an unhardened client and buys
nothing. The last resort becomes the hardened build with an `expect`, and no unhardened client can escape
the function. Pre-existing defect, fixed in the work package that already owns this function.

**D-011c — `3xx` is never retried and never followed, and this is stated because the ladder makes it
tempting.** With `Policy::none()` reqwest hands the 3xx back to `get`, where the `!status.is_success()` arm
turns it into `IndexHttpFailed` — correct today, and the loud-failure property D-011a is built on. The risk
the ladder introduces is not that reqwest starts following redirects; it is that an implementer holding a
3xx response *and* a fresh retry classifier in the same function takes the obvious next step and re-issues
against `Location`. That is **manual** redirect-following on an unguarded client (no `GuardedResolver`),
bypassing `resolve_base_url`'s plain-HTTP gate — the exact CWE-918/CWE-319 pair `Policy::none()` closes,
reintroduced one layer up where the policy cannot see it. A remote-controlled `Location` on a client with no
SSRF resolver is arbitrary egress; an `http://` `Location` is a silent scheme downgrade on a fetch whose
scheme was already gated. So the classifier names 3xx explicitly: **never retried, never followed**,
returned as `IndexHttpFailed` unchanged. C-016 asserts it — as written without that case, a manual-follow
implementation passes every contract in this ADR.

**D-011d — the memory arithmetic, which is why the outer cap is block-tier and not a nicety.**
`TAG_REFRESH_CONCURRENCY = 64` × `INDEX_REFRESH_CONCURRENCY = 8` = the 512 ceiling, and
`MAX_INDEX_DOCUMENT_BYTES = 32 MiB` is a **per-request allocation** ceiling accumulated into an in-memory
`Vec` (`ocx_index.rs:266-284`) — so 512 in-flight requests ceiling at **16 GiB resident**. That is true
today, but today's 60 s `.timeout()` bounds how long any one request can sit near its ceiling, so a hostile
peer cannot *hold* the high-water mark. Remove the total deadline with nothing in its place and a slow-drip
peer holds all 512 allocations at whatever size it chose, indefinitely, while never tripping a per-frame
idle bound. **The byte cap bounds the peak; only a deadline bounds the peak's duration, and duration is what
turns a peak into exhaustion.** The retry ladder compounds it on the traffic axis: a peer that serves
32 MiB − 1 and then resets produces a retryable transport error, so cumulative transfer per logical fetch is
up to 3× the cap. This is the arithmetic behind C-028, and it is why "the byte cap was always the real DoS
control" is only half true — it is the real *memory* control and was never a *time* control.

**The byte cap is what actually bounds memory, and it stays.** `ReqwestIndexTransport::get` already refuses
a declared oversize body before reading a byte, and enforces `MAX_INDEX_DOCUMENT_BYTES` incrementally while
streaming (`ocx_index.rs:248-284`). The total deadline was never the primary DoS control; the cap is.

**D-011a — `redirect::Policy::none()` is kept, and the open question is recorded.** The OCI spec says
clients SHOULD follow redirects and MUST NOT forward `Authorization` across hosts; but for *static* index
documents a redirect most likely means a CDN canonical-host hop (benign), a captive-portal/SSO intercept
(where refusing fails loudly and distinguishably, instead of parsing an HTML login page as JSON), or a
misconfiguration. Refusing keeps the second property, which is a real safety property in exactly the
environment #330 is about. **Unresolved:** whether `index.ocx.sh` (Cloudflare Pages) issues any redirect in
normal operation. That is answered by hitting the endpoint, not by more analysis — see §12.

### D-012 — HTTP/1.1 and connection pooling are left alone, deliberately

reqwest's `pool_max_idle_per_host` default is unlimited, and TLS-inspecting proxies commonly downgrade
non-browser HTTP/2 to 1.1. Neither is changed here. Setting `pool_max_idle_per_host` bounds *idle* sockets,
not concurrent in-flight connections, so it does not address the CONNECT-tunnel count; the thing that does
is the concurrency ceiling, which is A9's scope (§7). Recorded so a reviewer does not read the omission as
an oversight.

---

## 5. Testable component contracts

Numbered `C-001…`. **Two design records in this repo use colliding `C-`/`S-` series**: this ADR's own, and
`design_spec_servable_index_snapshot.md`'s. Every reference below to the *other* series is written
`snapshot-spec C-0NN`; a bare `C-0NN` always means this ADR. The collision is wholesale — `C-005`, `C-012`,
`C-013`, `C-014`, `C-022`, `C-024`, `C-027` and `S-001`…`S-010` all exist in both with different meanings —
so the prefix is load-bearing, not decoration.

Each is written so a tester can produce a failing test without reading the
implementation. **Every contract below either states a red-reachable condition or is explicitly labelled a
non-regression guard** — where the natural test would pass whether or not the code ran, the contract says
how to make it discriminate. The four labelled guards (C-002, C-005, C-015, C-027) are green *before* the
change by design: they exist to stay green, and a builder must not read their passing as evidence the fix
landed.

### Auth (fork)

**C-001 — A warm `ensure_auth` issues zero network requests.**
Given a stub registry that counts `GET /v2/` and token-realm requests, and a client configured with
`RegistryAuth::Basic`, when `ensure_auth(identifier, Pull)` is called twice for the same identifier: the
first call issues ≤ 2 requests; **the second issues 0**.
*Edge cases:* (a) `RegistryAuth::Bearer` — **a staleness assertion, not a count**: `auth()` with token A,
then `auth()` with token B, then the wire header carries **B**. "0 requests on both calls" is true whether
Bearer early-returns or falls through, so it cannot discriminate; D-001a excludes Bearer from the early
return and this is the test that proves it; (b) a registry answering `200` with no `WWW-Authenticate` —
1 then 0, and the 0 comes from **D-003a's host challenge cache** (the token cache stays cold on this path
by construction, `client.rs:882`), never from a "this registry needs no auth" latch; (c) a *different repository*
under the same registry — the second call issues a fresh token exchange (the key includes the repository);
(d) a different `RegistryOperation` for the same repository — fresh exchange.
*Red-reachability:* revert the cache consultation and the second call must issue 2 requests again. This
test is the whole regression guard for D-001, because **no existing test can see the bug** — all ~10
`ensure_auth` tests assert at the `OciTransport` trait boundary through an in-memory stub with zero real
HTTP, and none counts `GET /v2/` or token-exchange hits inside `_auth()`. Build it on
`oci/client/builder.rs`'s `StubRegistry`/`TcpListener` pattern, which is the only place in the tree that
counts real wire requests.

**C-002 — `ensure_auth` is still called exactly once per registry operation.** *(non-regression guard —
green today, and green is the whole point.)*
All ten exact-count tests in `crates/ocx_lib/src/oci/client.rs::tests::ensure_auth` pass **unchanged** —
the nine `assert_eq!(calls.len(), 1)` single-operation tests enumerated in D-001, plus
`client_ensure_auth_delegates_to_transport`'s 1-then-2. No new suppression is added at the
`ocx_lib::oci::Client` level, and no test in that module is edited by this plan.

**C-003 — `store_auth_if_needed` runs on every `auth()` call, cache hit or miss.**
Given a client whose `auth_store` is empty and whose token cache is pre-warmed for the key, when `auth()`
is called: `auth_store` afterwards contains an entry for that registry, and **zero** network requests were
issued.
*Why:* this is the 401-on-default-mode regression (`client.rs:5250-5253`). An early return placed *before*
`store_auth_if_needed` passes C-001 and reintroduces the bug.

**C-004 — Concurrent cold misses on one key produce exactly one token exchange.**
Given N=8 concurrent calls **through the coalesced acquisition seam** for one
`(registry, repository, operation)` against a counting stub whose token endpoint **holds** the response
until all 8 callers have arrived (a `Barrier`), the endpoint is hit exactly once and all 8 receive the same
token. The seam is exercised from **both** entry points — `auth()`'s post-D-001 miss path and
`get_auth_token()`'s miss path — because coalescing only the latter leaves the `index sync` stampede
untouched (D-003).
*Red-reachability:* without the hold, the leader can finish before the others arrive and the test passes on
serial execution — the hold is what makes the race observable. Precedent:
`pull_coordinator_coalesces_concurrent_same_digest_writers` uses exactly this counter + `Barrier` shape.
*Edge cases:* (a) the leader's exchange **fails** — every waiter observes an error, and the failure is not
cached as a permanent negative; (b) two different repositories concurrently — two exchanges, not one.

**C-005 — A cached 200 on `/v2/` never suppresses a later per-repository 401.** *(non-regression guard —
green before D-003 exists, because nothing latches the negative today; it guards against a mistake made
**inside** D-003/D-003a.)*
Given a registry that answers `GET /v2/` with `200` and answers a specific repository's manifest request
with `401 + WWW-Authenticate`, the client performs the challenge exchange for that repository.
*Assert **granularity**, not merely that a challenge happened:* repository A is served from the cached
host probe while repository B's `401` still triggers a full challenge — and, per D-003a's purge, B's `401`
drops the host entry so the next probe is re-derived rather than served stale. A test that only asserts
"a challenge happened" passes against a registry-wide latch, which is the bug.
*Why:* [containers/image#2754](https://github.com/containers/image/issues/2754) — caching at the wrong
granularity in the *other* direction.

### Index request path

**C-006 — A published refresh of one package issues exactly one root GET.**
Given a stub index source counting `GET p/<ns>/<pkg>.json`, when `refresh_tags` runs for a bare identifier
whose root lists T ≥ 2 tags with T distinct content digests: the root URL is requested **once**.
*Red-reachability:* today this count is `1 + min(T, 64)`. Assert `== 1`, not `<= 64`.
*Edge cases:* (a) a package whose root 404s — one request, and the miss is memoized (a second
`fetch_root_document` for the same repository in the same process issues nothing); (a2) a **foreign-registry
identifier** — `fetch_root_document` returns `Ok(None)` without issuing a request and memoizes **nothing**:
a subsequent `resolve_root` for the same *repository* under a served registry must still issue its GET.
*Red-reachability:* a tail-position insert makes the second call return the poisoned `None` with zero
requests — assert the request count, not the return value (D-004a); (b) a **non-404**
failure — nothing is memoized, and a repeat ask re-requests.

**C-007 — `check_format_version` fetches `config.json` at most once per process, per source.**
Given a stub counting `GET config.json` **that holds the response until all N callers have arrived (a
`Barrier`, the C-004/C-008 shape)**, an N-package refresh against one source requests it once.
*Red-reachability:* without the hold this test passes today — `check_format_version` is a
read-check-then-fetch (D-005), so whether N tasks each fetch is a scheduling accident and serial execution
gives a green that proves nothing. The hold is what makes the coalescing observable.
*Edge case, load-bearing:* an **absent** `config.json` (404) resolves to `assumed_v1()` and is
**deliberately not memoized** (C-005 of the snapshot spec — "so a tree that later publishes one is picked
up without restarting the process"). So the count-once assertion holds **only for a served document**; the
404 case asserts the opposite and must have its own test. A coalescing group that memoized the assumed-v1
result would break that contract silently.

**C-008 — Concurrent `resolve_root` for one repository produces one GET.**
Same `Barrier`-held-stub shape as C-004, at width 8 against a single repository.

**C-009 — An unchanged published re-sync issues zero dispatch-object fetches.**
Given a local index already holding every dispatch object a package's root names, when the package is
refreshed again against an unchanged source: **zero** `GET p/<ns>/<pkg>/o/<algo>/<hex>.json` requests.
*Edge cases:* (a) one tag repointed at a new digest — exactly one dispatch GET, for that digest; (b) a
single-platform (leaf) tag — no `o/` object exists by design, so it is fetched every run and that is
correct, not a miss.

**C-010 — A corrupt dispatch object is refetched and repaired, never skipped and never fatal.**
Given a dispatch object seeded at the target digest's path with **tampered bytes**, when the gated refresh
runs: the object is fetched from the source, `write_dispatch_object` overwrites it, and the file afterwards
hashes to the claimed digest. The command's exit code is unaffected by the tampering.
*Why this test must ship with D-006:* neither existing `persist_dispatch` test
(`..._writes_one_object_for_multi_platform_tag_without_recursion`,
`..._writes_nothing_for_single_platform_tag`) seeds a pre-corrupted object, and §D-006 established there is
no other backstop. Mirrors `write_dispatch_object_self_heals_a_tampered_existing_file` one layer up.
*Red-reachability:* changing the gate to `if let Ok(Some(_)) = …` (i.e. collapsing `Err` into skip) must
turn this red.

**C-011 — DESCOPED — D-007a fired, see `decisions_index_sync_perf_autonomous.md` §E.** *The derived path
skips the body for a held multi-platform tag.* A12 (the HEAD-then-skip mechanism this contract exercises)
was built in full, all six of its contracts written and exercised against a real socket, then reverted:
both of D-007a's descope triggers fired — the header-omitting-registry case below, and (independently) a
leaf-heavy tag population is a permanent regression, not merely an untested edge. Retained here as the
record of what was tested, not as a live obligation. **Edge case (d) is the one exception — it survives
A12 and stays a live guard**, because the risk it names is general to any future shape-inference shortcut,
not specific to A12's implementation.
Given a derived source and a local `o/` already holding the object for tag `t`'s current digest: refreshing
`t` issues one digest lookup and **zero** manifest-body GETs, and `t` is still recorded in the root.
*Edge cases:* (a) `o/` absent — the body is fetched (a leaf tag is indistinguishable from an unseen tag),
and the **total request count for that tag is 2** (HEAD + GET) against today's 1. Assert the count, not
just that a fetch happened: this was A12's regression on a leaf-heavy registry and no other contract could
see it (D-007);
(b) `o/` corrupt — the body is fetched and the object repaired; (c) a **reserved** tag (`__ocx*`,
`sha256.<hex>`) is filtered before any lookup, so it costs zero requests of either kind; (d) **an object
that hashes correctly but is not an image index** — a bare platform manifest seeded at the digest's path,
the shape a copied/distributed tree can carry — is **not** recorded as a version and routes to a fetch,
exactly as `Err(DigestMismatch)` does. The bytes are already in hand from `read_dispatch_object`, so the
`decode_index_manifest` check costs nothing; skipping it would silently record the wrong shape. Keep this
edge case even with A12 gone: it is the standing guard against any future implementation that infers a
dispatch object's shape from its mere presence in `o/` rather than decoding it.

**C-011a — DESCOPED — D-007a fired, see `decisions_index_sync_perf_autonomous.md` §E.** *A registry
omitting `Docker-Content-Digest` does not cause a double body fetch.* This is the descope trigger itself:
measured directly against the fork (`external/rust-oci-client`), a stub registry that omits the header
costs 1 HEAD + **2** manifest-body GETs against today's 1 GET — a registry that is already
non-conforming pays for A12's optimisation rather than benefiting from it. Given a stub registry that
answers HEAD without the digest header and GET with the manifest, refreshing one tag would need to issue
**at most one** manifest-body GET in total to be worth shipping.
*Red-reachability:* the naive D-007 implementation issues two (one inside `fetch_manifest_digest`'s
fallback, one from `fetch_manifest_raw_bytes`). Assert on the body-GET count, not on success.
*This contract could not be met without a fork signature change (surfacing the fallback GET's discarded
body), so A12 is descoped rather than shipped with the double fetch* (D-007a).

### Partial success

**C-012 — A failing tag does not discard its package's succeeded tags.**
Given a package whose root lists tags `a`, `b`, `c` with distinct content digests, and a source that fails
the dispatch fetch for `b`: after the refresh, the committed root pins `a` and `c` at their fetched digests,
does **not** pin `b`, and the call returns an error.
*Edge cases:* (a) **every** tag fails — nothing is committed and the error propagates; (b) `b`'s failure is
the *first* to complete but `a`'s is the lowest input index — the returned error is `a`'s (determinism);
(c) a tag only the local copy holds is still present afterwards (merge never deletes, C-014).

**C-013 — Tags aliasing a failed digest are excluded with it.**
Given tags `x` and `y` both pointing at content digest `D`, and a source that fails the fetch for `D`:
**neither** `x` nor `y` is pinned. (`refresh_published` dedups by digest, so only one of them is fetched;
the other must not free-ride on a fetch that never happened.)

**C-014 — A partial commit does not adopt package-level fields.**
Given a committed root with `repository = oci://old.example/ns/pkg` and a fetched root with
`repository = oci://new.example/ns/pkg` and one failing tag: after the refresh the committed
`repository` is still `oci://old.example/ns/pkg`.
*Complement:* with **no** failing tag, the fetched `repository` **is** adopted (unchanged
`RootScope::Package` behaviour). Both halves are asserted, or the test cannot tell "conservative" from
"broken".

**C-015 — The fan-out shape is unchanged.** *(non-regression guard.)*
`local_index.rs` contains exactly 2 occurrences of `buffer_unordered(` and exactly 2 of
`buffer_unordered(TAG_REFRESH_CONCURRENCY)`. `index_common.rs` contains exactly 1 `log::error!` and
exactly **2** `log::warn!`, each with its sanitizer argument individually asserted.

**Both counts are over the module's non-test half with comment lines stripped, exactly as the guards
themselves count** — `local_index.rs:1280-1287` splits on `#[cfg(test)]` and drops `//` lines (the file
also carries `buffer_unordered(500)` in a doc comment at `:1274` and two needle literals at `:1289`/`:1295`);
`index_common.rs::module_code()` (`:523-530`) does the same, and the raw file has 3 matching lines per
needle against the production half's 2 and 1. A builder who greps the raw files gets different numbers and
would report a false "the guard is already broken".

**The guard that actually covers D-008's file is
`local_index.rs::the_per_tag_fan_out_is_sized_by_the_constant_at_every_site` (`:1279-1300`), and it is
weaker than it looks.** `index_common.rs`'s two guards cannot see `local_index.rs` at all: the denylist
scan roots at `crates/ocx_cli/src/command/`, and `the_funnel_neutralizes_both_halves` reads
`include_str!("index_common.rs")` — one file in another crate. So a `JoinSet` added *alongside* the two
surviving `buffer_unordered` calls in `local_index.rs` keeps both counts at 2 and ships green. C-026 closes
the half of this gap that matters for this plan.

### Retry and timeout

**C-016 — A retryable status is retried; a non-retryable one is not.**
Given a stub index endpoint answering `503` once then `200`, `get` returns the body and issues 2 requests.
Given a stub that answers `200`, streams **part** of the body, then aborts the connection on attempt 1 and
serves the full body on attempt 2, `get` returns the **complete** bytes and issues 2 requests — this is the
case that discriminates a whole-`get` ladder from a `send()`-only one (D-010).
Given one answering `404`, `get` returns `IndexFetch::NotFound` after **1** request. Given one answering
`403`, `get` returns an error after **1** request.
*Edge cases:* (a) `401` on the index path is *not* retried by the transport ladder (see C-020 for the
registry-side token-refresh rule, which is a different mechanism). (b) A **`302` with a `Location`
header** returns an error after **1** request and **no dereference of `Location`** — assert the stub's
redirect target is never requested. Without this case a manual-follow implementation passes every other
contract here (D-011c).

**C-017 — `Retry-After` is honoured in both wire forms.**
Given `429` with `Retry-After: 2`, the client waits ≥ 2 s before the retry. Given `429` with
`Retry-After: <HTTP-date 3 s in the future>`, it waits ≥ 3 s. Given a second `429` whose `Retry-After`
counts *down*, the second wait uses the **new** value, not the first one seen.
*Edge cases, all three red-reachable and none optional (D-010's clamp):* (a) `Retry-After: 86400` — the
call **returns within the run's bound**; it does not sleep for a day. Without the clamp this assertion
hangs, which is the red; (b) an HTTP-date in the **past** — the wait is zero, never a negative or wrapped
duration; (c) an **unparseable** value — the wait is zero and the normal jittered backoff applies.
*Red-reachability:* a test that only asserts "a retry happened" passes with `Retry-After` ignored entirely;
the assertion must be on the elapsed interval. **Use virtual time — `tokio::time::pause()` plus
`tokio::time::Instant::now()` — for C-017, C-018 and C-019 alike.** Real time makes C-017 cost ≥ 3 s and
C-018 ≥ 20 sleeps and both timing-flaky; virtual time discriminates "slept 2 s" from "returned immediately"
deterministically and instantly. This is the single clock mechanism for all three contracts.

**C-018 — Backoff is jittered.**
Over 20 sampled first-retry delays with `base = 250 ms`, at least two distinct values are observed and all
fall in `[0, 250 ms]`.
*Red-reachability:* deterministic backoff produces 20 identical values and fails the distinctness
assertion.

**C-019 — The retry budget is a ratio, shared across every clone of the transport.**
Given a fan-out of **N ≥ 2 packages** against a source that fails every request with `503`, the total
number of *retry* requests issued across the whole run does not exceed `max(10, 0.1 × total_requests)`,
even though each individual request is independently eligible.

*Lifetime, and it is the red-reachability condition:* `ReqwestIndexTransport` is `#[derive(Clone)]` over a
`reqwest::Client` (`ocx_index.rs:160-163`), `IndexTransport` requires `box_clone` (`:146`), and
`index_common.rs:142` builds `index::Index::from_source(source.clone())` **inside** the fan-out. A budget
held as a plain field is therefore per-clone, i.e. **per package** — and a single-package test passes
either way. The counters must be `Arc<AtomicU64>`-shaped so they survive `box_clone`, and the test must fan
out across ≥ 2 packages through cloned transports or it measures nothing.
*Edge case:* once the budget is exhausted, subsequent failures return immediately — asserted on **virtual**
elapsed time under `tokio::time::pause()` (see C-017), because a budget that still sleeps is the
amplification the budget exists to prevent, and under a paused clock a wall-clock assertion cannot tell the
two apart.

**C-020 — A `401` on a registry-derived request refreshes the token and retries once.**
Given a registry that answers the first manifest request `401 + WWW-Authenticate` and the retried one
`200`, the operation succeeds with exactly one token exchange and one retry. A second consecutive `401`
after a fresh token is an authentication failure (exit 80), not a retry loop.
*Home:* this is **not** an index-transport contract — D-010b binds it to the registry-derived half, and
there is zero `ensure_auth` under `oci/index/**`. It belongs beside the token cache, in the fork work
package, alongside D-003a's purge-on-401 (which is the same mechanism seen from the challenge layer).

**C-028 — The outer cap bounds a single attempt, and its absence is detectable.**
Given a stub that streams one byte every `idle_bound − ε` — never tripping the per-frame idle bound and
never approaching `MAX_INDEX_DOCUMENT_BYTES` — `get` fails at roughly the outer cap.
*Red-reachability, and it is the whole point of the contract:* remove `.timeout(outer_cap)` from
`build_index_http_client` and this test must hang past the cap and fail on the harness deadline. C-021
alone cannot see this — both of its halves pass identically whether or not an outer cap exists, which is
the "unchecked green" shape. The dribbling peer is a real exposure, not a hypothetical: the body loop's
only other bound is the 32 MiB byte cap, and a slow-drip peer never reaches it in any human timeframe;
multiply by the fan-out and `ocx index sync` does not terminate. Today's hard 60 s deadline is what
guarantees it does, so removing it without this contract is a regression the test suite could not see.
*Scope:* the cap wraps **one attempt** (`ClientBuilder::timeout`), not the retry ladder — D-011 states why.

**C-021 — An honest slow body is not aborted; a stalled one is.**
Given a stub that streams a small index document at one byte every 500 ms for 90 s (exceeding the old 60 s
hard cap) with no gap longer than the idle bound: `get` **succeeds**. Given a stub that sends headers and
then nothing: `get` fails with a timeout after roughly the idle bound.
*Precedent:* `oci/client/builder.rs::read_timeout_tests` already builds a stalling `TcpListener` registry
and asserts the transient classification; this is that pattern applied to the index transport.
*Edge case:* the `MAX_INDEX_DOCUMENT_BYTES` cap still fires on an oversize body regardless of pacing —
asserted separately, because relaxing the total deadline must not relax the byte cap.

### Bounded fan-out

**C-022 — `ocx index catalog --tags` bounds its in-flight requests.**
Given a stub registry listing 100 repositories, with a per-request **hold** long enough for overlap to be
observable, the peak concurrent in-flight tag listings does not exceed `CATALOG_TAG_CONCURRENCY` (16) by
more than a small, measured margin.
*Red-reachability, measured directly on this fixture rather than borrowed:* the snapshot spec's S-004
calibration (20 ms / 200 ms) does not carry over. At 200 ms, with the bound raised to 512, the fixture
peaked at 25–35 against a threshold of 32 — the check passed on unbounded code some of the time. At 500 ms
the same widened build peaked at 37, 48 and 55 over three runs, while the real bound held at 16 nine runs
in ten and 17 once. **Assert `peak <= CATALOG_TAG_CONCURRENCY + 8`, not equality and not 512**: the +8
slack absorbs the catalog listing's own trailing handler still winding down as the first tasks start (the
one observed 17), while 24 sits comfortably below every widened-build peak seen, so it still discriminates.
Include the non-vacuity preconditions: every repository was listed, each listing actually resolved its
tags, and `peak_in_flight > 1` (the snapshot spec's S-004 precondition). Test:
`test/tests/test_index.py::test_index_catalog_tags_bounds_its_in_flight_listings`.

---


### Contracts added by the review panel

**C-023 — Concurrent *cold* `ensure_auth` for one key produces one token exchange.**
Given N=8 concurrent `ocx_lib::oci::Client::ensure_auth` calls for one identifier against a counting stub
whose token endpoint holds until all 8 arrive (the C-004 `Barrier`), the endpoint is hit **once**.
*Why it is separate from C-001 and C-004:* C-001 tests *sequential* calls ("called twice") and C-004 tests
the fork primitive directly. Neither covers the shape `ocx index sync` actually produces — N cold callers
entering `auth()` at once — which is what S-006 claims to fix. Without this contract, D-003 could be
implemented at `get_auth_token` only and every existing assertion would still pass (D-003).

**C-024 — `IndexHttpFailed` carries the status as a typed field, and the classifier reads it.**
Given a stub index endpoint answering `503`, the resulting `IndexError::IndexHttpFailed` exposes the status
structurally (not as `format!("unexpected status {status}")` inside a `Box<dyn Error>`), the retry
classifier's decision is computed from that field, and **`ExitCode` classification is unchanged: 69
(`Unavailable`), exactly as `error.rs:283` gives today.**
*Why it is a contract and not a cleanup:* D-010a calls it a required sub-change, and a required change with
no contract is one nothing notices if it is skipped.
*Red-reachability:* revert the field and the classifier stops compiling; the exit-code half is a
non-regression assertion and is labelled as one.

**C-025 — The `GET /v2/` challenge probe is issued once per host, and a 401 purges it.**
Given a counting stub registry and a sync touching **three distinct repositories** under one host: the
`GET /v2/` probe is issued **once** in total (not once per repository), and each repository still performs
its own token exchange (C-001 edge case (c) is unchanged).
*Edge case, and it is the one that matters:* after the cached challenge is in place, a repository answering
`401 + WWW-Authenticate` with an `error` parameter causes the **whole host entry** — challenge and every
scoped token under it — to be dropped, and the next request rebuilds it. Without this, a host-level cache
reintroduces containers/image#2754 at coarser granularity.
*Red-reachability:* today the probe count equals the repository count; assert `== 1`, never `<= N`.
*If D-003a is descoped, this contract is descoped with it and §13's request-count claim is restated.*

**C-026 — The retry ladder and the self-heal add no new operator-facing line.**
A **per-site** guard over the *current* inventory, counted over each module's comment-stripped non-test half
(the `local_index.rs:1280-1287` preprocessing shape, copied — not a raw grep). Measured, so a rebaseline is
visible in the diff:

| Module | production window | `log::info!` | `log::warn!` |
|---|---|---|---|
| `local_index.rs` | above `#[cfg(test)]` at `:904` | **1** (`:196` refreshing tags) | **2** (`:516` unparseable derived root, `:873` source published without `config.json`) |
| `ocx_index.rs` | above `#[cfg(test)]` at `:1223` | **0** | **5** (`:196` client build fallback, `:1012` yanked, `:1022`/`:1023` deprecated, `:1029` superseded) |

Each site's argument is asserted individually, per the
`index_common.rs::the_funnel_neutralizes_both_halves` precedent — a new site needs its own assertion block,
never a count bump.
*This contract was originally written as "zero occurrences", which was false in both files before any of
this work existed.* Recorded because the failure mode of the wrong version is worse than a red test: a
builder driving `ocx_index.rs` to zero would delete the yank, deprecation and supersede signals, which are
load-bearing publisher semantics, and one driving `local_index.rs` to zero would delete the unparseable-root
warning. The property is *"this change adds none"*, not *"this file has none"*.
*Why:* S-003 ("retries are `debug!`") and S-007 ("no warning — this is a self-heal") both restate a hard
project invariant, and **no existing guard covers either file** (C-015 documents why: `index_common.rs`'s
funnel guard reads one file in another crate). A `log::warn!("retrying {url}")` added during this work
would otherwise ship green.
*Red-reachability:* add one `log::warn!` to either file and the count assertion must fail. Include the
non-vacuity precondition the sibling guards use — assert the scanned window is non-empty and does **not**
contain `#[cfg(test)]`, so a truncation bug cannot fake a zero count.

**C-029 — A token inside its renewal margin is a cache miss.**
Given a `TokenCache` entry expiring in **1 s**, `get` returns `None` (a miss) and the caller
re-authenticates; given one expiring in **10 minutes**, `get` returns the entry. Both halves are asserted —
one alone cannot tell a working margin from a cache that never hits.
*Second half, and it is a different code path (D-001e):* a token endpoint answering with **10 s** of life
yields a token that is **not** returned as usable — the margin binds at acquisition, not only at
`TokenCache::get`. `auth()` returns the freshly minted token directly after `tokens.insert()`
(`client.rs:865-870`) and never reads back through `get`, so a margin implemented only in `get` is
invisible to the leader *and* to every coalesced waiter under D-003.
*Edge case:* a **backwards** wall-clock step resolves to "expired" and re-authenticates; it does not panic.
`token_cache.rs:160`, `:195`, `:211` currently `.expect("Time went backwards")` (D-001d).
*Why it is required, not hygiene:* D-001 removes the accidental per-call refresh that masks a sub-second
entry today (D-001c).

**C-030 — The dispatch gate is `read_dispatch_object`, and no existence check can creep in.**
A source-text guard over `local_index.rs`'s comment-stripped non-test half, with **both** halves:
*negative* — zero occurrences of `.exists()`, `try_exists`, `path_exists_lossy`, `symlink_metadata`,
`metadata().is_ok()`; *positive* — `read_dispatch_object` appears in the refresh gate.
*Why both:* a negative-only guard still passes when the needle stops matching anything, so it reads as
coverage while providing none. Strip comments first, or a denylist that quotes the forms it forbids matches
its own comment.
*Why a guard at all:* D-006 makes the entire self-heal property rest on one line and says so; a rule that
important with no enforcement is the asymmetry this ADR already flags for `JoinSet`.

**C-031 — Every new diagnostic in the retry region redacts the URL.**
A source-text guard asserting that each `log::debug!` site added inside `ReqwestIndexTransport::get`'s
region passes `redact_url(...)`, checked **per site** rather than as a count.
*Why:* an index base URL may embed `user:password@` — that is what `redact_url` (`ocx_index.rs:101-117`,
whose doc names CWE-532) exists for, and every existing `IndexHttpFailed` construction in `get` already
uses it (`:226, 242, 253, 271, 277`). A retry ladder's natural line — `"retrying {url}, attempt 2/3"` — is
a new emission site in exactly that region.
*Why per-site and not a count:* a count budget is satisfied by one raw call paired with one redacted call
elsewhere. `index_common.rs::the_funnel_neutralizes_both_halves` is the precedent — it checks each site's
sanitizer argument individually and says why.

**C-027 — Index reading semantics are untouched.** *(non-regression guard — the paranoia clause.)*
No item in this ADR changes any of the following, and each is asserted directly rather than argued in prose:
(a) **local-first resolution** — `ChainedIndex` still answers a tag from the local copy before contacting a
source, and `persist_dispatch`'s four production callers (two of them on the ordinary resolve path) behave
identically, which is what D-006's placement in `refresh_published` buys;
(b) **offline capability** — with the offline gate set, `ocx index sync` still refuses before any network
contact and every resolve that can be answered locally still is; no new failure mode is introduced by the
retry ladder, whose first attempt cannot be reached offline;
(c) **`ocx index update`'s remote default** — unchanged flag grammar, unchanged default source selection,
unchanged pin-moving set.
*Red-reachability:* **none — these are non-regression guards**, in the same class as C-002, C-005 and
C-015, and the §5 preamble's blanket claim does not bind them. An earlier draft named a discriminating
mutation ("revert D-006's gate into `persist_dispatch` and a resolve test goes red"); that mutation **does
not compile**, because the digest is not available there, which is the entire reason for the placement. The
one reachable mistake it named — collapsing `Err(DigestMismatch)` into skip — is already C-010's red. What
C-027 buys is a standing assertion that the read paths still behave, checked on every run; it is not
evidence any fix landed.

## 6. Rejected options — recorded with the quoted reason

### 6.1 A2 — Per-package root-digest diff against the remote catalog. **DROPPED.**

The proposed local-vs-remote root-digest diff is not merely similar to the deleted F2 mechanism — **it is
F2**, matching the ADR's own (superseded) spec of it verbatim:

> `adr_index_indirection.md:565-602` — "**F2. Catalog sync — digest diff.** `ocx index catalog` / `ocx index
> update` against a published index diffs the remote per-package root digests against the **local** catalog
> and: re-snapshots … only the packages whose root digest moved… The local `c/index.json` is both the
> offline catalog-listing source … and the diff basis for the next sync — the previous file's per-package
> digests are compared against the freshly-fetched remote map, **so no separate validator store is
> needed**."

That last clause is decisive. F2's original design **already** used `sha256(local root bytes)` against a
freshly-fetched, unpersisted remote map. So "pure in-memory comparison, persists nothing" is not a novel
variant that escapes the objection — it is F2's own shape, re-derived.

**Why F2 died is soundness, not storage:**

> `adr_index_indirection.md:1069-1073` — "Under merge semantics the local root's bytes legitimately diverge
> from the remote's (local-only tags), so the old digest diff would report **every package as permanently
> stale**".

Read precisely: "restoring it needs a recorded last-observed-remote digest" describes what a *different*,
hypothetical **working** staleness report would require (remote-now vs remote-then). It is not what made
F2 broken. F2 was broken **on its own terms, storage-free**, because merge semantics make
`local digest != remote digest` the **steady state** — local-only tags the remote stopped listing, package-
level fields, per-tag `observed` timestamps. Any one trips a whole-document hash. "Deleted, not narrowed"
is explicit: the ADR retires the digest-diff *approach*, not just its persistence.

The comparison is **safe but useless**: its only error direction is a false "changed" (a harmless extra
refresh), and false-changed is the steady state for any package with local-only history. For an actively
maintained index the skip would essentially never fire.

**A performance win here must come from reducing the fixed per-package cost of a no-op refresh (round trip
+ parse), never from predicting no-op-ness.** That is exactly what D-004, D-005 and D-006 do.

### 6.2 Conditional GET / ETag / last-seen-remote-root-digest. **REJECTED, twice over.**

Independently re-proposed during this initiative's research phase from apt/dnf/crates.io/Go prior art, as
"ask *did upstream change*, not *does my merged copy match upstream*". The distinction is real — old-remote
vs new-remote is a different comparison from local vs remote — and it is still refused, because it requires
caching a last-seen remote root digest as a separate field, and:

1. `adr_index_indirection.md:572` retired the conditional GET by amendment on 2026-07-30, naming the
   sidecar's category directly.
2. `adr_index_indirection.md:1073` refuses **every** home for such a field — "in the catalog envelope, in a
   sidecar (the `.etag` file's category, already rejected), or in machine-global state that desyncs from a
   shipped index home."
3. `index_store.rs::commit_removes_a_stale_etag_sidecar_left_by_an_older_ocx` actively **deletes** such
   sidecars.
4. The OCI distribution spec has **zero** hits for `ETag`/`If-None-Match` on any pull path; the only
   conditional mention is opt-in, push-side, for referrers-list manifests. So even the registry half would
   be per-implementation guesswork.

**Why the prior art does not transfer, in one line:** apt, dnf, Nix, crates.io and Go all assume the local
copy is a *mirror of a past server state, never independently edited*. OCX's local root is **authored** —
a merge of remote tags with locally-held ones that are never dropped. It is not a point on the remote's
timeline at all, so there is nothing to diff against, at any granularity. No cleverer algorithm (rsync
rolling checksums, structural diff, merkle-tree-of-fields) fixes a false premise.

### 6.3 A13 — Memoizing the catalog parse. **DROPPED as moot.**

The `ponytail:` deferral in `index_store.rs` is about **per-resolve** re-parsing on the read path
(`read_root`), not about `index sync`'s catalog fetch. A sync reads the remote catalog once per
invocation and does not touch that cost class. Nothing to revisit.

### 6.4 "Trust the path on the hot path; add a periodic scrub". **REJECTED.**

Proposed in research §4, on the premise that re-hashing per check cancels the optimization. **The cost
premise is wrong** — see the table in D-006: the alternative to a local hash is a network fetch *followed
by* a hash, because `write_verified_object` hashes on every write as a CWE-345 check. Hashing locally costs
strictly less at every object size, up to and including the 32 MiB DoS cap.

The periodic scrub/repair subsystem that argument implies is therefore both unnecessary and YAGNI: the
gated path self-heals by construction (`Err(DigestMismatch)` → fetch → overwrite). And `Path::exists()`
was independently ruled out by `index_store.rs`'s own design constraint, because it strands corruption
permanently.

### 6.5 `moka`, `tokio::sync::OnceCell`, `dashmap`, `futures::Shared` for coalescing. **REJECTED.**

- **`moka::try_get_with`** — a failed init leaves the key absent so the next caller retries. Backwards
  here: a confirmed 404 is a positive, load-bearing result that must be cached and broadcast. Getting
  there means wrapping `Option<T>` as the success type and treating "confirmed absent" as `Ok(None)` — at
  which point the repo's own `Acquisition` enum has been re-derived, worse, with an eviction policy nobody
  needs for three fields.
- **`tokio::sync::OnceCell`** — a cancelled leader silently promotes a waiter to retry the init. Two
  callers of one logical operation can then exit with different codes. `Group`'s `Abandoned` broadcast
  gives every waiter the same outcome.
- **`dashmap::entry()`** — the guard is a synchronous shard lock; holding it across the `await` risks a
  documented deadlock, so the realistic pattern is lock → check → drop → await → lock → insert, which is
  the racy shape already in `ocx_index.rs`. It buys map thread-safety, not coalescing.
- **`futures::Shared`** — needs a hand-written `HashMap<K, Shared<…>>` wrapper with leader tracking, i.e.
  `Group` reimplemented; and dropping all clones before completion cancels the inner future with no
  abandonment signal.

**`utility::singleflight::Group` is used** — rung 2 of the ladder (reuse what is in the codebase), not
rung 5 (a new dependency).

### 6.6 Coalescing/short-circuiting at the `ocx_lib::oci::Client` level instead of in the fork. **REJECTED.**

Eight tests assert an exact `ensure_auth` call count of 1 per operation. Skipping the transport call when
a local check says "already authed" drives that count to 0. The fix location is forced by the test surface,
which independently confirms the fork as the right home (D-001).

### 6.7 Copying `go-containerregistry`'s transport-level auth model. **REJECTED as a model.**

Its `bearerTransport` holds **one** bearer credential per transport instance — not a map — and scopes only
ever accumulate (`newScopes = append(newScopes, bt.scopes...)`), so a transport's scope set grows
monotonically. Two issues document the resulting pain, both closed without a fix:
[#1744](https://github.com/google/go-containerregistry/issues/1744) (asks for scope-keyed tokens, partly for
the **security** reason that a leaked cumulative token grants every scope ever seen) and
[#740](https://github.com/google/go-containerregistry/issues/740) (a user listing many tags found "every
`remote.Image` call is going through its own auth flow"). Recorded so nobody converges on it by accident.
**containerd's `authorizer.go` is the model instead** — per-host handler cache, per-scope-string token
cache nested under it, `WaitGroup`-per-key coalescing.

---

### 6.8 A TTL over the local root's own `observed` timestamp. **REJECTED.**

The one re-derivation of A2 that uses **only local authored state** — "do not refetch a root observed in
the last N minutes" — and therefore slips past §0.1's "every home" objection entirely. It would genuinely
cut root GETs on repeated syncs. Named and rejected here because otherwise someone proposes it in review
and the ADR has no recorded answer.

It is rejected on **operator intent, not on storage**. `ocx index sync` is an explicit operator act whose
entire meaning is "move my pins to what the source says *now*". A TTL makes a re-run inside the window
silently do nothing — no error, no diagnostic (the silence principle forbids one), no pin movement — which
breaks the operator's mental model far worse than a wasted GET costs. It also makes the command's result
depend on wall-clock history rather than on the source's state, which is §0.3's determinism property
inverted. A wasted request is a performance defect; a sync that quietly declines to sync is a correctness
one.

## 7. Deferred scope, and the evidence for reversing it

**A9 (thread `--jobs`/`OCX_JOBS` into the index fan-out) and A10 (a `[registry]` config surface) are
deferred to [ocx-sh/ocx#333](https://github.com/ocx-sh/ocx/issues/333), behind
[#324](https://github.com/ocx-sh/ocx/issues/324).** This is the owner's scoping decision, taken
deliberately. This section exists so that decision is **reversible on evidence** rather than by
re-argument.

### What argues for pulling A9 forward

1. **Docker's own default is 3, and its guidance is "lower it".** `dockerd --max-concurrent-downloads`
   defaults to **3** layers in flight per pull, with the daemon docs stating: *"If you are on a low
   bandwidth connection this may cause timeout issues and you may want to lower this."* OCX's stated
   ceiling is **512**. That is the most widely deployed OCI-adjacent client officially recommending
   concurrency reduction for exactly the symptom class #330 reports.
2. **reqwest will not self-limit.** `pool_max_idle_per_host` defaults to `usize::MAX`; there is no
   "max total connections" knob. Concurrent in-flight connections are effectively unbounded client-side,
   so OCX's own limiter is the only backpressure that exists.
3. **HTTP/2 does not survive TLS inspection.** Zscaler ("non-browser HTTP/2 … downgraded to HTTP/1.1 …
   when the non-browser HTTP/2 configuration is kept disabled"), Juniper ("the proxy removes h2 from the
   list of protocols"), Palo Alto, Fortinet and Broadcom all document the same behaviour, commonly
   off by default. Under HTTP/1.1 there is no multiplexing: 512 logical requests become up to 512 TCP+TLS
   connections through the proxy — each of which the intercepting proxy terminates **twice**, roughly
   doubling its handshake CPU cost.
4. **A pull-through cache amplifies rather than absorbs.** Harbor's proxy cache does **not** coalesce
   concurrent cache-miss requests for the same artifact — N simultaneous pulls trigger N independent
   upstream fetches ([goharbor/harbor#22570](https://github.com/goharbor/harbor/issues/22570), maintainer-
   acknowledged as an open request, not current behaviour). The mental model "the corporate cache will
   smooth the burst out" is inverted: on a cold cache, client concurrency is multiplied upstream.

### What argues against, and why the deferral is defensible anyway

- **A9 is not free in guard cost.** `index_common.rs::the_stated_ceiling_is_the_product_of_the_two_real_constants`
  asserts `INDEX_REFRESH_CONCURRENCY * TAG_REFRESH_CONCURRENCY == 512` from the **live** constants; a
  runtime value has nothing fixed to multiply. And
  `there_is_exactly_one_fan_out_and_no_join_set` asserts the call site literally names the constant
  (`buffer_unordered(INDEX_REFRESH_CONCURRENCY)`), which stops matching under
  `buffer_unordered(concurrency)`. **Both need rewriting, not relaxing** — they assert against the constant
  by identity. That is genuine design work (what does a ceiling *mean* when it is configurable?), and it is
  the right kind of work to do behind #324's foundation rather than inside a fix.
- **A9 binds a hard constraint if it does land.** Both #316 and #167 independently record it: threading a
  concurrency limit into a nested fan-out must introduce a **separate permit class**, never reuse the
  package-manager one — an ancestor holding a permit while waiting on children that cannot acquire one is a
  deadlock. #316: *"the ancestor-deadlock argument … does not apply to a separate verify-scoped
  semaphore."* #167: *"a dedicated layer-extract `Semaphore`, a separate permit class from the package one
  (deadlock-safe)."*
- **This plan reduces the request count substantially without touching the ceiling.** D-001/D-003 remove
  2 of every 3 auth requests; D-004/D-005 remove `1+min(D,64)` redundant root GETs per package; D-006
  removes every dispatch fetch for an unchanged published package. Fewer requests at the same width is a
  smaller burst. It is plausible — untested — that #330's failure does not survive those cuts.

### Reversal trigger, stated so it can be acted on without re-litigating

**Pull A9 forward into this plan if, after D-001/D-004/D-005/D-006 land, `ocx index sync` still fails
against the reporting environment.** That is a measurement, not a judgement call, and it is the only thing
that distinguishes "the burst was the problem" from "the request count was the problem". If it fires, the
minimal form is a *constant reduction* (Option D) rather than the full `--jobs` surface — a one-line change
with no guard rewrite — with the configurable surface still going to #333.

### A12 (WP7) — descoped, and how to reopen it

Unlike A9/A10, A12 was **built, contracted against six tests, and measured before being dropped** — see
`decisions_index_sync_perf_autonomous.md` §E. It is recorded here in the same reversible shape so a later
re-argument does not have to reconstruct why, rather than being re-litigated from D-007's rationale alone.

**Measured request counts**, from two stub registries differing only in whether the HEAD response carries
`Docker-Content-Digest`:

| stub registry | HEADs | manifest-body GETs |
|---|---|---|
| sends `Docker-Content-Digest` | 1 | **1** |
| omits it | 1 | **2** |

**Measured against a real tag population, today vs. with A12:**

| tag shape | today | with A12 |
|---|---|---|
| held multi-platform | 1 GET | 1 HEAD — saves the body, not the round trip |
| cold tag (absent from `o/`) | 1 GET | 1 HEAD + 1 GET — **+1** |
| leaf (single-platform) | 1 GET | 1 HEAD + 1 GET — **+1, every run, permanently** (a leaf never writes to `o/`, so its gate is cold forever) |
| registry omitting `Docker-Content-Digest` | 1 GET | 2 body GETs |

So on a registry whose tags are predominantly single-platform, A12 made `ocx index sync` **slower**, and on
a non-conforming registry it was strictly worse. It is a win only for held multi-platform tags on a
conforming registry, and even there it saves bytes rather than a round trip.

**Reversal trigger.** Reopen only if the fork's `fetch_manifest_digest`
(`external/rust-oci-client/src/client.rs:1048-1052`) changes its return type to surface the fallback GET's
discarded body — a second fork change, worth making only if the fork is being touched for another reason
anyway. Absent that fork change, both descope triggers stand and A12 stays out. C-011(d) is the one part of
its contract set kept regardless — it guards against a *future* implementation inferring a dispatch
object's shape from presence in `o/` rather than decoding it, which is a general risk, not specific to A12.

---

## 8. UX scenarios

Numbered `S-001…`. Action → expected outcome → error cases. No scenario below changes the CLI grammar;
`ocx index sync`'s surface (snapshot-spec C-012) is untouched by this ADR.

**S-001 — Re-syncing an unchanged published registry.**
*Action:* `ocx index sync ocx.sh` on a machine whose local index already holds every package the catalog
lists, unchanged.
*Outcome:* exit 0. Per registry: 1 `config.json` + 1 `c/index.json`. Per package: 1 root GET, **0** dispatch
GETs. Nothing under `$OCX_HOME/index` is rewritten — `CatalogTransaction::commit` writes nothing when the
merged map equals what it read, and `merge_root` returns `None` when nothing in scope changed, so the tree
is byte- **and** mtime-identical.
*Errors:* an unreachable source fails that registry's enumeration and the exit is non-zero (snapshot-spec C-013's
authoritative stop, unchanged).

**S-002 — One tag's dispatch object cannot be fetched from the source.**
*Action:* `ocx index sync corp.example` where one package's root lists a tag whose `o/` object is
**refused** (e.g. `403`).

> **A `404` is the wrong fixture here, and the reason is a real ambiguity, not a test detail.**
> Measured on the implemented branch, a 404'd `o/` object exits **0 and pins the tag**. On the published
> wire an absent `o/` object is indistinguishable from a **single-platform (leaf) tag**, whose `content` is
> its own manifest digest and which by design never has an `o/` object at all. So "absent" cannot mean
> "failed" without breaking every leaf tag. This is the same `Ok(None)` ambiguity D-007 records for the
> derived path; A12 was to own it there and is now descoped, so on the published path the ambiguity simply
> stands. A non-404 refusal is the honest way to express "this object exists and could not be had".

*Outcome:* every other tag of that package is pinned; the failing tag is not; the package-level
`repository` field is left as committed (D-008c); the command exits non-zero with that package's error
reported once through the shared funnel. Other packages are unaffected (snapshot-spec C-012 Aggregation, already the
contract).
*Errors:* if **every** tag fails, nothing is committed for that package — not even an empty root — and the
error propagates identically to today (D-008f).

**S-003 — A transient 503 mid-sync.**
*Action:* `ocx index sync ocx.sh` where the CDN answers `503` for a handful of requests.
*Outcome:* each is retried with full-jittered backoff (250 ms base, 3 s cap, ≤ 3 attempts) and the sync
completes with exit 0. No operator-facing warning per retry — retries are `debug!`.
*Errors:* if the run's global retry budget is exhausted, subsequent failures return immediately without
sleeping, and the run fails with the lowest-index failure.

**S-004 — A rate-limited registry answers 429 with `Retry-After`.**
*Action:* `ocx index sync <acr-registry>` at scale.
*Outcome:* the client waits the server-stated interval (both `delay-seconds` and HTTP-date forms honoured)
and retries. A second `429` uses its own, possibly smaller, `Retry-After`.
*Errors:* past the attempt cap or the run budget, the failure surfaces with the status intact
(D-010a), and the exit code is **unchanged at 69 (`Unavailable`)**, exactly as `error.rs:283` gives today.
D-010a's status field exists for the *retry classifier*, not to reclassify the exit code: §10.4 promises no
exit-code change, exit codes are the CLI surface other tools `case $?` on, and nothing in #330 needs the
distinction. Recorded as a decision, not an omission (C-024).

**S-005 — A slow-but-progressing corporate link.**
*Action:* `ocx index sync corp.example` through a throttling proxy where a root document takes 90 s to
arrive but never stalls.
*Outcome:* the fetch **succeeds**. Today it fails at the 60 s hard deadline.
*Errors:* a connection that goes genuinely silent still fails at the idle bound; an oversize body is still
refused at `MAX_INDEX_DOCUMENT_BYTES` regardless of pacing.

**S-006 — A private registry with Basic credentials.**
*Action:* `ocx index sync private.corp` over N packages.
*Outcome:* one `GET /v2/` + one token exchange for the first operation against that
`(registry, repository, operation)`; subsequent operations on the same key issue neither. Concurrent
first-contact requests share one exchange.
*Errors:* a credential rejection is still `AuthenticationFailure` → exit 80 on the first attempt, not
retried. A `401` *after* a previously-good token (expiry mid-batch) refreshes once and retries; a second
consecutive `401` is exit 80.

**S-007 — A corrupt local dispatch object.**
*Action:* `ocx index sync ocx.sh` on a machine where one `o/…json` was truncated by a crash.
*Outcome:* that object is refetched and repaired; every other object is skipped; exit 0. No warning — this
is a self-heal, and per feedback a common benign state gets debug + self-heal, not a WARN.
*Errors:* if the refetch fails, that tag is excluded (S-002) and the exit is non-zero; the corrupt file
remains and is repaired on the next successful run.

**S-008 — `ocx index catalog --tags` against a large registry.**
*Action:* `ocx index catalog --tags` where the registry lists hundreds of repositories.
*Outcome:* tag listings run at a bounded width instead of one task per repository. Output, ordering and
exit semantics are unchanged (results keyed into a `BTreeMap`, lowest-input-index failure wins).
*Errors:* unchanged — a per-repository failure exits non-zero; a task panic still aborts the rest and
propagates.

**S-009 — `ocx index sync --dry-run`.**
*Action:* unchanged by this ADR.
*Outcome:* enumeration runs; the refresh loop does not; nothing under `$OCX_HOME/index` is opened for
write; the patch-descriptor piggyback does not run (snapshot-spec C-027). **The retry ladder applies to
the published half of enumeration** — `fetch_catalog_strict` goes through `ReqwestIndexTransport::get`,
where D-010 puts the ladder. **Derived enumeration does not**: `IndexImpl::list_repositories` hits the
registry's `/v2/_catalog` through a different client, outside this ADR's transport seam. Qualified rather
than widened, because open question 3 (published or derived at the reporting site?) is unanswered and
widening the seam on a guess is how scope grows.
*Errors:* `--frozen`/`--offline` still refuse the whole command → exit 81, ahead of any fetch.

**S-010 — `ocx package install` / `ocx package exec` on a warm store.**
*Action:* any ordinary resolve.
*Outcome:* **unchanged and silent.** No new diagnostic, no comparison, no drift line. A locally-committed
resolve still performs no network access. The only observable difference is that a pull which *does* reach
the network burns 2 fewer auth requests per layer.

---

## 9. Pin-safety check — every item against `subsystem-oci.md`

> *"Name the command the user ran, and the package they named. If the diff can move a pin (a tag's
> `content`, or a root's `repository`) for anything outside that set, it is wrong however well-motivated
> the fetch is."*

| Item | Can it move a pin? | Mechanism |
|---|---|---|
| **A1a** fork `auth()` cache | **No** | Auth only. No index write path is reachable from `auth()`/`_auth()`. |
| **A1b** token coalescing | **No** | Same. Coalescing changes *how many* exchanges happen, never what is resolved. |
| **A3** dispatch gate | **No** | Skips a fetch for a digest the **committed root already names**. It cannot introduce a pin, only avoid re-fetching an object for one that exists. Strictly reduces writes. |
| **A4** `fetch_root_document` caches | **No** | Populates an in-process memo with the document the caller was already given. The set of packages fetched is unchanged. |
| **A5** retry ladder | **No** | Retries the same idempotent GET for the same URL. A retry cannot address a different package. |
| **A6** timeout semantics | **No** | Changes when a fetch gives up. |
| **A7** partial commit | **Narrower than today** | It commits **fewer** tags than the all-or-nothing path would on success, never more, and only for the package the enumeration named. D-008c additionally withholds the `repository` half on partial success. |
| **A8** singleflight | **No** | Deduplicates concurrent fetches of one key. Every caller receives what it would have fetched. |
| **A11** bounded catalog fan-out | **No** | `index_catalog.rs` is read-only — it never calls `refresh_tags`. That is why it is exempt from the C-024 guard. |
| **A12** derived HEAD-then-skip | **No** | Same argument as A3, with the digest learned by HEAD instead of read from a root. `records_root_tag` still gates what is recorded. |

**Aggregate check.** The set of packages `ocx index sync` touches is decided entirely by
`enumerate_catalog` (snapshot-spec C-013 — live from the source), and no item above changes it. `ocx index update`'s set
is argv. Neither is widened.

**Silence check (`adr_index_indirection.md:1048-1059`).** No item adds a diagnostic to the resolve path.
D-008 explicitly adds no new `log::warn!` (D-008d); retries and self-heals log at `debug!`.

**Determinism check.** D-008e makes the returned per-package error the lowest-input-index failure rather
than the first to complete. A3/A12 skip a fetch only when the local object hash-verifies against the digest
the root pins, so the resolved answer is identical warm or cold.

---

## 10. Migration and rollout

### 10.1 The fork change (A1a, A1b)

- **Where:** `ocx-sh/rust-oci-client`, branch `ocx/integration`, currently pinned at `485d2a2` as the
  submodule `external/rust-oci-client`.
- **Commit convention:** conventional-commit scoped to fork modules — `fix(client): …` for `auth()`,
  `feat(client): …` or `fix(token_cache): …` for the coalescing primitive.
- **ocx-side tracking issue:** titled `fix(oci): fork — auth() re-runs the full handshake on every call`,
  labelled `area/oci` + `tech-debt`. Precedent: [#270](https://github.com/ocx-sh/ocx/issues/270),
  [#271](https://github.com/ocx-sh/ocx/issues/271), [#272](https://github.com/ocx-sh/ocx/issues/272).
  Closest template for the *shape* of this change is `9c1e5c7 feat(client): injectable dns_resolver on
  ClientConfig (ocx SSRF pin seam)` — an ocx-driven seam added to the fork.
- **Note the adjacency:** [#270](https://github.com/ocx-sh/ocx/issues/270) is open against the same
  `native_transport` retry area A5 touches. Land order should avoid a conflict; they are independent
  changes to adjacent code.

### 10.2 How fork behaviour gets pinned from the ocx side — the load-bearing part

**The fork's own test suite is excluded from the ocx workspace and never runs in CI.** A test added
alongside the fork change proves nothing from ocx's perspective; it can rot, be reverted, or be lost in a
rebase onto upstream, and ocx's gate stays green.

Therefore the pin is an **ocx-side wire test**, and it is a ship-blocking part of A1, not a follow-up:

1. **C-001 is the pin.** A test in `crates/ocx_lib/` that stands up a real `TcpListener` stub registry
   (the `oci/client/builder.rs::push_wire_tests` / `read_timeout_tests` pattern — the only place in the
   tree that counts real wire requests), calls `Client::ensure_auth` twice for the same identifier, and
   asserts the second issues **zero** requests.
2. **It must be red-reachable against the fork.** Before landing, run it against the *pre-change* submodule
   commit and observe it fail. Record that in the PR body. Without this the test is indistinguishable from
   one that never ran — a green that cannot go red is not a check.
3. **The submodule bump and the wire test land in the same ocx commit.** A bump without the test leaves the
   window §3 Option C warned about; the test without the bump is red. One commit closes both.
4. **C-003 pins the side effect** the same way — an ocx-side assertion that `auth_store` is populated after
   a cache-hit `auth()`, so a future fork rebase that "simplifies" the early return past
   `store_auth_if_needed` fails ocx's gate rather than the fork's.

### 10.3 Landing order

| Wave | Work | Rationale |
|---|---|---|
| **0** | *(no standalone seam wave)* | The `RetryPolicy`/`TransportHardening` value objects ship **in the same commit as their first and only consumer** (D-010/D-011). A wave for a struct with no consumer, no contract and no possible red test is the "unchecked green" shape at the process level; §3 A′ needs the seam to *exist*, not to land first. |
| **1a** | Fork `auth()` + coalescing; submodule bump; C-001/C-003/C-004 wire tests | Largest measured win; independent of everything else; fixes layer pulls too. |
| **1b** | A4 + A8 (`ocx_index.rs`, `oci_index/cache.rs`) | File-disjoint from 1a. |
| **1c** | A3 + A12 + A7 (`local_index.rs`, `RootScope`) | Single file; A7's `RootScope` change touches the same functions A3 gates, so one work package. |
| **1d** | A11 (`index_catalog.rs`) | Fully disjoint. |
| **2** | Acceptance-tier measurement: re-run the reporting environment | Feeds §7's reversal trigger. |

Waves 1a–1d are file-disjoint and parallel-capable behind wave 0.

### 10.4 What does not change

No CLI grammar, no exit code, no wire format, no persisted format, no `ocx.toml`/`ocx.lock` key. **No
changelog-visible interface break.** Per `CLAUDE.md`, the changelog entry is the commit subject; nothing
here needs a `!`.

Every existing guard listed in §5 C-015 and §7 stays green, with two exceptions that must be updated in the
same commit as their cause:

| Guard | Cause | Action |
|---|---|---|
| `merge_root` / `commit_published_root` unit tests (`local_index.rs` ×5) | D-008b changes `RootScope::Tag(&str)` → `RootScope::Tags(&[&str])` | Update the five test call sites (`local_index.rs:3268,3299,3325,3333,3365`); the assertions themselves are unchanged. |
| **Four** production `RootScope::Tag` sites, not two | D-008b | **Two are mechanical** — the constructors at `local_index.rs:228` and `chained_index.rs:785` take a slice. **Two are semantic and are the hard part**: `local_index.rs:243`, where `RootScope::Tag(named) => *tag == named` becomes set membership and which decides *which tags `commit_published_root` persists dispatch objects for*; and `local_index.rs:1041`, the `merge_root` arm D-008b exists to generalise. Describing the whole change as "a slice at the call sites" would send a builder into the two load-bearing lines expecting mechanics. |
| `index_sync.rs::an_enumerated_repository_becomes_a_bare_identifier` | Mentions `RootScope::Tag` in **comments and failure messages only** — its needles are `clone_with_tag`/`tag_or_latest`/`clone_with_digest` | No functional change; update the prose so it names the surviving spelling. |
| `no_index_module_outside_this_one_grows_a_refresh_fan_out` exemption list | Only if `index_catalog.rs` is renamed (it is not) | None expected — recorded because the guard asserts `seen_exempt.len() == exempt.len()` and fails on a stale name. |

---

## 11. Constitution check — `arch-principles.md` and the rule set

| Principle | Where it binds | Verdict |
|---|---|---|
| **Facade / Composite root** — `Index` is a type-erased `Box<dyn IndexImpl>`; new mechanism lives behind the seam | A8's `Group` is a field on `OcxIndex`/`OciIndex::Cache`, behind `IndexImpl`. A3/A12 gate **in `refresh_published`/`refresh_derived`, before the `persist_dispatch` call** — not inside `persist_dispatch`, which has two resolve-path callers (D-006). | **Pass** — nothing bypasses the facade. |
| **Utility Catalog rule** — "If new helper broadly applicable, upstream to `utility/`" | A8 reuses `utility::singleflight::Group`; no second dedup mechanism in ocx_lib. | **Pass.** |
| — | A1b writes a coalescing primitive **inside the fork**, which cannot reach `crate::utility`. | **Deviation, justified:** crate boundary. Recorded in D-003. It is not a second mechanism *in ocx_lib*. |
| **Locking Policy** — atomic-rename-replaced data locks via `lock_scoped` into `$OCX_HOME/locks`, never a persistent sidecar | Nothing in this plan adds a lock or a sidecar. A7 writes through the existing `begin_catalog_transaction`. | **Pass.** |
| **`ChainMode` / `LocalWritePolicy` gate** — a cache optimisation must not let a `Query` path start writing | A3/A12 only *skip* writes. A4/A8 populate read caches. The exhaustive `match self.mode` in `walk_chain` and the `IndexOperation::{Query,Resolve}` split are untouched. | **Pass** — verify at review that no `Group` is shared across a policy boundary (D-005 states the rule). |
| **Silence principle** — no comparison/drift diagnostics on the default resolve path | D-008d forbids a new warn; retries and self-heals are `debug!`. | **Pass.** |
| **Core vs Plugin boundary** — index sync is core-binary behaviour | No plugin split proposed. | **Pass.** |
| **Don't Own Non-Domain Code** (`quality-core.md`) — Warn-tier, Block for wire formats | The retry ladder and the fork's coalescing primitive are hand-rolled rather than taken from a library. | **Deviation, justified.** The tempting argument — "a library would make five mechanisms, not four" — does **not** hold and is not used: importing `reqwest-middleware`+`reqwest-retry` (which implements `Retry-After` in both wire forms, full jitter and status classification over the same `reqwest::Client` D-011 configures) would make five *sites* today and one shared *mechanism* the other four could migrate onto, which is what #324 wants. The two reasons that do hold: (a) the Tech Strategy Golden Path commitment this ADR's own metadata records — **no new dependency** for this work; (b) two behaviours no library ships — the run-global retry **ratio** budget (D-010 rule 2), and ACR's `Retry-After` counting *down* across polls, which forbids caching the first value seen. For the fork's primitive: it cannot depend on ocx's `utility/`, and `async_singleflight`-style crates are closure-based (the future must be built *before* leadership is known) and unaudited. Neither is a wire format, so neither is Block-tier. |
| **No premature abstraction** (`quality-core.md` KISS/YAGNI) | The "one client factory" is scoped to a **policy value object**, not a factory trait, precisely so it is deletable. §3 A′ states the reversal. | **Pass.** |
| **Unbounded fan-out is Warn-tier** (`quality-rust.md` async) | A11 closes the last unbounded one in the `index` family. A9's ceiling is deferred (§7). | **Partial** — deferral recorded with its reversal trigger. |
| **`MutexGuard` across `.await` is Block-tier** | `OciIndex::Cache`'s deliberate no-outer-lock design is preserved (a `Group` beside the maps, never around them). `Group` uses `tokio::sync::Mutex` and holds it only across the map operation. | **Pass** — assert at review. |
| **Verification honesty / "unchecked green"** (`quality-core.md`) | Every contract in §5 states its red-reachable condition; §10.2 makes red-before-green a landing requirement for C-001. | **Pass by construction.** |
| **Stability tiers** (`CLAUDE.md`) | Internal structure only. `RootScope` is an internal enum — rename in place, no compat shim (D-008b). | **Pass.** |

---

## 12. Open questions

**Q1 — Does `index.ocx.sh` (Cloudflare Pages) issue redirects in normal operation? — ANSWERED, 2026-08-22.**
Measured directly (orchestrator, `curl -sS -D -`): both `https://index.ocx.sh/config.json` and
`https://index.ocx.sh/c/index.json` answer `HTTP/2 200` with **no redirect**. `redirect::Policy::none()` is
therefore safe against the production index in normal operation, and **D-011a stands unchanged**. Reopen
only if a canonical-host hop is introduced later.

> **Two observations from the same measurement, recorded so they are not mis-read later.**
>
> 1. **Cloudflare *does* serve `etag` on both documents**, alongside
>    `cache-control: public, max-age=0, must-revalidate`. A conditional GET would work at the HTTP layer.
>    This is **not** grounds to reopen §6.2: the 2026-07-30 retirement was a *design* decision about
>    persisting a per-machine validator inside a tree that is otherwise served-or-content-addressed, never
>    a claim that the server lacked support. Server capability was never the constraint, so observing it
>    changes nothing.
> 2. `c/index.json` is **12,279 bytes**. This corroborates the amendment's own arithmetic — "a catalog
>    measured in kilobytes, with the round trip paid either way" — and confirms the ETag would have bought
>    a 304 over a 200 on a 12 KB body. The saving was never the point and still is not.

**Q2 — Does 512-concurrent through a forced-HTTP/1.1 TLS-inspecting proxy actually reproduce #330?**
Discovery could not determine this statically and flagged it as needing a live test rather than a claim.
This is §7's reversal trigger. It is the single measurement that decides whether A9 belongs in this plan.

**Q3 — Do Artifactory and Nexus pull-through repositories coalesce concurrent cache misses, or fan out
N-to-N like Harbor?** No vendor documentation or issue was found either way. If they behave like Harbor,
the case for A9 strengthens materially for exactly the corporate deployments #330 concerns.

**Q4 — Should a resolve re-consult the source for a yank published *after* the pin was taken?**
`subsystem-oci.md` records this as an open semantics question (it trades against the silence principle and
against invariant 2). Nothing in this ADR changes the answer; noted because A3's skip makes the "no network
on an unchanged tag" property stronger, which sharpens the question.

**Q5 — What does a "stated ceiling" mean once concurrency is configurable?**
Two guards assert the 512 ceiling as a compile-time product of two constants. A9 makes one of them a
runtime value. Answering this is prerequisite work for #333 and is deliberately not answered here.

---

## 13. Consequences

**Positive.**
- A steady-state re-sync of an unchanged published registry drops from `R×P×(1 + min(D,64) + D)` package-
  level requests to `R×P×1`. On a 200-package registry with 5 tags each, that is roughly 2,200 requests →
  200.
- Auth cost drops from 3 requests to 1 for a **repeated** operation on an already-touched
  `(registry, repository, operation)` — which is where the bulk of the waste is: it removes 2 wasted
  requests **per layer** on every package pull, a benefit entirely outside this issue's scope. For the
  *first* touch of each of P distinct packages in one sync, D-001+D-003 alone give 3 → 2 (the token
  exchange is per-repository by design); **D-003a's host-level challenge cache is what takes that to 1**,
  and if D-003a is descoped this line is restated to 3 → 2 for the first-touch case rather than left
  standing. Stated precisely because the unqualified "3 → 1" overclaims for exactly the workload this ADR
  targets.
- A transient failure no longer discards a package's completed work, and no longer fails the run at all if
  it is retryable.
- A slow-but-progressing link stops being a hard 60 s failure.

**Negative / accepted.**
- One new hand-rolled coalescing primitive exists in the fork (crate boundary; D-003) — and the cost is
  **recurring, not one-time**: it is new divergence from upstream that every future rebase of
  `external/rust-oci-client` must carry, in a fork whose issue tracker is disabled (§10.1 records the
  ocx-side tracking workaround). Trying `OnceCell` before the watch-map (D-003) is partly a bet on
  shrinking this surface.
- **Worst-case wall time grows** (D-011): today a document fails at 60 s; after this change a pathological
  path can run ~15 minutes per document, and the run carries no wall-clock bound by design. A caller with a
  CI job timeout is the intended backstop. This is the one change here that can make a reported experience
  *worse* rather than better, and it is accepted knowingly — #330's complaint is failure, not slowness.
- `RootScope`'s shape changes, touching its call sites and `merge_root`'s tests (D-008b).
- **A12 was descoped, not shipped.** Its payoff was honestly small — one HEAD replacing one GET for held
  multi-platform tags only — and on a registry whose tags are predominantly **single-platform** it would
  have been a net **regression**: a leaf tag costs HEAD + GET where today it costs one GET, every run,
  because a leaf never has an `o/` object to hit. That was a second descope trigger on D-007a, alongside the
  header-omitting-registry double fetch C-011a measured; both fired, so A12 did not ship. See
  `decisions_index_sync_perf_autonomous.md` §E for the measured request counts.
- The concurrency ceiling is unchanged, so the burst-shaped half of #330's hypothesis is untested until
  §7's measurement runs.

**Neutral.**
- No user-visible surface changes. No changelog break.
