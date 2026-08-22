# Research: OCI Distribution Spec + Proxy Failure Modes for `ocx index sync`

Context: `ocx index sync` against a corporate registry is slow and fails when one
operation times out. Current implementation: index-document fetches use a hard
60s total-request deadline + `redirect::Policy::none()`; registry fetches use a
120s per-frame idle bound; up to 512 requests in flight; no retry on the index
path.

Status: COMPLETE.

---

## 1. OCI Distribution Spec — what does it actually require?

### 1a. Token authentication — challenge per request, or cacheable?

- The normative spec (`opencontainers/distribution-spec`, `spec.md`, §"Determining
  Support" / the `GET /v2/` endpoint) says only: *"This endpoint MAY be used for
  authentication/authorization purposes, but this is out of the purview of this
  specification."* Auth mechanics are explicitly **not** specified by the
  distribution-spec itself.
- The actual mechanism in universal use is the separate **Docker Registry Token
  Authentication** spec (originated in `docker/distribution`, now maintained at
  [distribution.github.io/distribution/spec/auth/token/](https://distribution.github.io/distribution/spec/auth/token/)
  and mirrored at [docs.docker.com/reference/api/registry/auth/](https://docs.docker.com/reference/api/registry/auth/)).
  Flow: registry responds `401` with `WWW-Authenticate: Bearer realm=...,service=...,scope=...`
  → client exchanges that challenge for a bearer token at the auth realm → client
  attaches `Authorization: Bearer <token>` to the retried request **and to
  subsequent requests**.
- **Token TTL is short by design and not registry-controlled by the client**:
  per the token spec, `expires_in` is *"(Optional) The duration in seconds since
  the token was issued that it will remain valid. When omitted, this defaults to
  60 seconds."* and *"For compatibility with older clients, a token should never
  be returned with less than 60 seconds to live."* So a conformant client cannot
  assume more than 60s of validity unless the server advertises more.
- **Caching a token per `(registry, scope)` is conformant and is exactly the
  documented client model** — the spec's own diagram has the client reuse "the
  original request" with the token attached, and describes tokens as *"opaque
  Bearer token[s] that clients should supply to subsequent requests."* There is
  no requirement to re-challenge `GET /v2/` before every request; the challenge
  is per distinct `(realm, service, scope)` need, not per request. Real clients
  (containerd, `oci-client`, `crane`, `skopeo`) all cache the token until it
  expires or a request against the same scope 401s.
- **Practical implication for `ocx index sync`**: if a sync batch takes longer
  than ~60s wall-clock (very plausible at 512-way concurrency behind a slow
  corporate link — see §3), tokens acquired at the start of the batch may expire
  mid-flight for requests still queued. A conformant client must treat `401` on
  an in-flight request as "re-auth and retry," not as a hard failure — this is a
  retry case the current no-retry index path does not have (§4).

### 1b. `_catalog` pagination

- **`_catalog` is not part of the normative OCI `distribution-spec` at all.**
  Verified directly against `spec.md`: no `_catalog` endpoint appears in the
  Endpoints table or in the Pull/Push/Content-Discovery/Content-Management
  sections. `extensions/README.md` in the same repo confirms this explicitly:
  `_catalog` (and `_oci`) are listed as **"Reserved prior extension"**
  namespaces — reserved so future extensions can't collide with it, precisely
  *because* it predates the extension mechanism and isn't itself normatively
  specified.
- `_catalog` lives only in the older **Docker Registry HTTP API V2** spec
  (now the CNCF "Distribution" reference implementation docs,
  [distribution.github.io/distribution/spec/api/#listing-repositories](https://distribution.github.io/distribution/spec/api/)),
  which registries implement by convention, not OCI conformance obligation:
  - `GET /v2/_catalog?n=<int>&last=<repo>` — `n` limits page size, `last` is the
    exclusive lower bound for the next page (same cursor idea as tag listing).
  - Pagination is **recommended, not required**: *"For registries with a large
    number of repositories, this response may be quite large. If such a
    response is expected, one should use pagination."*
  - A registry **may ignore or override `n`**: *"A registry may also limit the
    amount of responses returned even if pagination was not explicitly
    requested."* No maximum value for `n` is specified anywhere — it's
    registry-defined. Clients cannot assume a requested `n` is honored; they
    must drive off the `Link` header (RFC 5988, `rel="next"`), not off getting
    back exactly `n` results.
  - **Conclusion**: since `_catalog` has zero OCI-conformance backing, an OCI
    registry is free to not implement it, cap `n` silently, or omit it under a
    read-only/anonymous-pull configuration (this is common in locked-down
    corporate registries). `ocx` should not depend on unpaginated or
    fully-paginated `_catalog` behavior against an unknown registry.

### 1c. Tag listing pagination — `GET /v2/<name>/tags/list`

- This one **is** in the normative distribution-spec (§"Listing Tags" /
  endpoint `end-8a`/`end-8b`). Verified text: `n` = number of tags requested,
  `last` = tag value to resume after: `/v2/<name>/tags/list?n=<int>&last=<tagname>`.
- Same non-mandatory shape as `_catalog`: *"A `Link` header MAY be included in
  the response when additional tags are available."* If present it *"MUST be
  set according to RFC 5988 with the Relation Type `rel=\"next\"`."* No stated
  maximum for `n`; a registry MAY return fewer than `n` results even without
  signaling truncation via `Link` only when the true count is `< n` — meaning a
  client can't infer "no more pages" purely from `len(results) < n` without
  also checking for an absent `Link` header.

### 1d. Redirects — normatively permitted, and does refusing them break anything for *index* documents?

- Verified spec text (`spec.md`, introductory "API" section, applies to all
  endpoints): *"Registries MAY respond to any request with a redirect per
  RFC 9110 (§15.4); clients SHOULD follow such redirects, and MUST NOT forward
  `Authorization` headers across host boundaries unless explicitly configured
  to do so."*
- This is exercised in practice almost exclusively for **blob GETs** — ECR,
  GCR/Artifact Registry (historically), and various self-hosted registries
  backed by S3-compatible storage issue `307` to a pre-signed object-storage
  URL so the registry itself doesn't proxy multi-GB layer bytes. This is
  well-documented folklore-turned-standard-practice; it is *not* separately
  specified beyond the generic redirect permission above.
- **Why the redirect ban is specifically safe for cross-origin auth**: browsers
  and (per spec text above) conformant registry clients strip `Authorization`
  on a cross-host redirect. If a corporate proxy or registry issues a redirect
  to a different host for a request that needed the original token, refusing
  the redirect outright (current `ocx` behavior for index docs) never loses
  correct behavior — following it wouldn't have worked with credentials anyway,
  and unauthenticated blob-storage redirects don't need them.
- **For index documents specifically (static files, not the registry API)**:
  refusing redirects is a different bet than for the registry API, because
  index docs generally aren't behind a signed-URL indirection scheme — they're
  static files served directly. A redirect on a *static* GET request is far
  more likely to mean one of: (a) benign — a CDN's canonical-host redirect
  (e.g. `www` → apex, or path-based root redirect), (b) a corporate captive
  portal / SSO-login redirect intercepting the connection, or (c) an actual
  configuration issue (moved index). Refusing (b) fails loudly and
  distinguishably (a redirect status instead of parsing an HTML login page as
  JSON), which is a real safety property. But refusing (a) turns a
  recoverable, single-hop redirect into a hard failure for a legitimate CDN
  reason unrelated to the corporate-proxy problem this project is solving.
  **Could not establish** whether ocx's actual index host (Cloudflare
  Pages/`index.ocx.sh` per this repo's tech stack) issues any redirects in
  normal operation — that's answerable by hitting the real endpoint, not by
  spec research; flagging rather than guessing.

### 1e. Conditional requests — ETag / If-None-Match

- **Not specified at all** for manifest or blob GETs. Full-text search of
  `spec.md` for `ETag`, `If-None-Match`, `If-Match` returns **zero hits** on
  any pull path. The only "conditional" mention in the whole document is under
  Referrers-API backwards compatibility, about **pushing** a referrers-list
  manifest: *"Clients MAY use a conditional HTTP push for registries that
  support ETag conditions to avoid conflicts with other clients"* — opt-in,
  push-side, and unrelated to read-path caching.
- What every pull response **is** required to carry is `Docker-Content-Digest`
  (verified in Pull/Push sections) — a content hash, not a cache-validator
  header, though it's usable as one by a client that already knows the
  expected digest (skip re-fetch if the digest matches what's cached).
- For **static index files** (not registry API), ETag/If-None-Match is a
  generic HTTP/CDN feature, not an OCI concern — whatever the hosting layer
  (Cloudflare Pages, S3, GitHub raw) does natively. **Could not establish**
  specifics for the actual ocx index host from spec research; this needs an
  empirical check (`curl -I` against the real endpoint) rather than more
  literature search.

---

## 2. Rate limiting across major registries

| Registry | Documented limit | Token exchange counted separately? | Signal on limit |
|---|---|---|---|
| **Docker Hub** | Anonymous: 100 pulls/6h per IPv4 or IPv6 /64. Free authenticated: 200 pulls/6h. Pro/Team/Business: unlimited. ([docs.docker.com/docker-hub/usage](https://docs.docker.com/docker-hub/usage/)) | Not documented either way in current docs. | `429`; pull-limit responses include a longer body linking to docs (not just a bare 429). Community-observed (not in the primary doc) `RateLimit-Limit`/`RateLimit-Remaining` headers with a `w=21600` (6h) window param. |
| **GHCR** | No published numeric pull quota for public images (effectively unlimited/no artificial throttling tied to a GitHub account). Community-reported general API throttling around 2,000 req/min authenticated GitHub API-wide — **not** confirmed as GHCR-registry-specific in official docs. | Unconfirmed. | `429` with `retry-after` observed in community reports (e.g. push-side "toomanyrequests: retry-after: …, allowed: 2000/minute"). |
| **Amazon ECR** | Per-API-action throttling, not a blanket pull quota: `GetAuthorizationToken` 20 TPS (burst 200 TPS); `BatchGetImage` sustained up to 1000 req/s after a 2020 quota raise (5–10x prior limits). Actual image-layer GETs go through the redirected S3 URL, outside ECR's own throttle. | **Yes, explicitly separate** — `GetAuthorizationToken` has its own independent TPS quota from image-pull operations like `BatchGetImage`. | `429`/`ThrottleException`; AWS's own guidance is retry with backoff, or request a Service Quota increase. |
| **Azure Container Registry (ACR)** | Fully documented, SKU-tiered token-bucket limits (Basic/Standard/Premium): DataplaneRead 10k/20k r/m per registry, 5k/10k r/m per identity; DataplaneWrite 2k/4k and 1k/2k; DataplaneDelete 1k/4k and 0.5k/2k; ListReferrers 0.5k/2k and 0.25k/1k; **OAuth 10k/20k r/m per registry**. ([learn.microsoft.com/azure/container-registry/container-registry-skus](https://learn.microsoft.com/en-us/azure/container-registry/container-registry-skus)) | **Yes, explicitly its own bucket** ("OAuth" category = auth/token exchange), tracked per-registry only (no per-identity sub-limit) and separately from DataplaneRead. A request can also count against *two* buckets at once (e.g. ListReferrers is both ListReferrers and DataplaneRead). | `429` + `Retry-After` in seconds, **dynamically decreasing** across repeated throttled polls (documented explicitly — don't hardcode the first value). Token-bucket refill means short bursts are absorbed; sustained excess is throttled for up to a full minute per burst. |
| **Quay.io** | Not a pull-count quota; a small per-IP requests/second ceiling with burst tolerance, enforced "in the most severe circumstances" (documented as low tens of req/s per IP). Anonymous pulls are not restricted beyond this. | Not documented separately. | `429`. |
| **Harbor / Artifactory / Nexus** | Self-hosted — limits are operator-configured, not vendor-documented ceilings. | N/A (operator's own auth, if any). | Operator-defined. |

**At 512 concurrent requests**, ACR (DataplaneRead per-identity 5k–10k r/m ≈
83–167 r/s sustained, token-bucket-bursty) and Quay (low tens of req/s per IP)
are the ones most likely to visibly throttle a single sync run; Docker Hub's
6-hour pull windows would only bite on repeated/large syncs, not burst
concurrency per se. **ACR is the one registry that documents, in writing, that
token exchange and data pulls are billed against independent quotas** — so a
retry/backoff design must treat 429-on-token-exchange and 429-on-blob/manifest-GET
as distinct failure classes with separate budgets, not conflate them.

---

## 3. Corporate proxy / TLS-intercept failure modes (the practical heart of the issue)

This section is explicit about **documented** vs **folklore-but-consistent**
vs **could not establish**.

### 3a. What breaks first under a few hundred concurrent HTTPS connections through a TLS-intercepting proxy

- **Documented, mechanistic**: A TLS-intercepting proxy terminates *two* TLS
  connections per client request (client↔proxy, proxy↔origin) instead of one
  end-to-end connection. Each full TLS 1.2/1.3 handshake costs on the order of
  single-digit milliseconds of CPU for asymmetric key-exchange operations
  (RSA/ECDHE) — cited figures around 7.8–9.2ms CPU/handshake, and this cost is
  roughly **doubled** by interception (proxy does the crypto work twice). At
  512 concurrent new connections this is real, additive CPU load on the
  **proxy**, not just the client or origin — a resource most client-side
  tooling has no visibility into and cannot budget for.
- **Documented**: many enterprise TLS-interception products (Zscaler, Fortinet,
  Netskope/Cloud SWG-class secure web gateways) **strip or mishandle the ALPN
  extension**, forcing negotiated protocol down to HTTP/1.1 even when both real
  endpoints support HTTP/2 — Zscaler specifically is called out for this in
  community/vendor threads (see refs). This matters directly for `ocx`'s
  concurrency model: HTTP/1.1 has no multiplexing, so every one of the 512
  in-flight logical requests becomes (up to connection-pool limits) its own
  TCP+TLS(x2) connection through the intercepting proxy, rather than sharing a
  small number of multiplexed HTTP/2 streams. This is the single most direct
  mechanism by which "512 concurrent requests" turns into "hundreds of
  concurrent TLS handshakes hammering the proxy," which is a plausible root
  cause of both slowness and cascading per-request timeouts.
- **Could not establish** hard per-vendor concurrent-connection ceilings (e.g.
  "Zscaler drops the Nth connection from one client") — this is proxy-product-
  and deployment-specific, not published as a general number, and is the kind
  of detail that would need to be measured against the specific corporate
  proxy in question rather than looked up.

### 3b. Documented guidance/defaults from container tooling about concurrency behind corporate proxies

- **Documented, direct precedent**: Docker's own daemon reference
  (`dockerd`) ships a `--max-concurrent-downloads` flag, **default 3** layers
  in flight per pull, with the daemon docs explicitly noting: *"If you are on a
  low bandwidth connection this may cause timeout issues and you may want to
  lower this."* This is Docker (the most widely deployed OCI-adjacent client)
  officially recommending **turning concurrency down, not up,** as the fix for
  proxy/low-bandwidth timeout symptoms — directly analogous to `ocx`'s
  512-in-flight setting.
- containerd/Moby have open, unresolved issues (e.g. `moby/moby#53081`,
  `containerd/containerd#2195`) asking for a *global* transfer-concurrency
  cap, separate from per-pull concurrency — the ecosystem consensus (from
  issue discussion, not a spec) is that **per-operation** concurrency knobs
  aren't sufficient; what matters is total concurrent transfers system-wide,
  which is closer to `ocx`'s 512 cap being a single global number today.
- **Could not establish** a documented numeric recommendation (e.g. "N per
  proxy" or "N per corporate network") from any container tool for the
  corporate-proxy case specifically; the Docker guidance above is the closest
  documented anchor, and it's qualitative ("lower it"), not a specific number.

### 3c. Pull-through cache behavior under high concurrency for cache-miss requests

- **Documented as a known limitation, not a designed serialization**: Harbor's
  proxy-cache project does **not** coalesce concurrent cache-miss requests for
  the same artifact — GitHub issue `goharbor/harbor#22570` documents that N
  simultaneous pulls of an uncached image each trigger **N independent upstream
  fetches** (thundering herd), with Harbor's team acknowledging the desired
  behavior (single upstream fetch, others served once cached) as an open
  feature request, not current behavior. **This means client-side concurrency
  does NOT turn into queueing at the Harbor cache layer — it turns into
  N-fold amplification of upstream load on a cache miss**, which is the
  opposite of what a naive mental model ("the cache will smooth it out")
  would predict.
- **Could not establish** the equivalent behavior for Artifactory or Nexus
  pull-through/proxy repositories with primary-source authority (no vendor doc
  or issue found stating whether they single-flight concurrent cache misses or
  also fan out N-to-N like Harbor). Flagging this as an open question rather
  than assuming either behavior — it would need direct testing against the
  target corporate registry.
- **Practical implication**: if the corporate registry in the failure reports
  is a Harbor-style pull-through cache (common for corporate Docker Hub/GHCR
  mirroring), high client concurrency on a cold cache is actively harmful —
  it doesn't get "absorbed," it gets multiplied.

### 3d. Timeout shape: total-request deadline vs idle/read timeout for a slow-but-progressing link

- **Documented pattern, from a mature and directly analogous tool (`curl`)**:
  curl offers both models as *separate, composable* knobs precisely because
  they solve different problems — `--max-time` (`CURLOPT_TIMEOUT`) caps total
  wall-clock for the whole operation regardless of progress, while
  `--speed-limit`/`--speed-time` (`CURLOPT_LOW_SPEED_LIMIT`/`_TIME`) aborts
  only when throughput drops below N bytes/sec for a sustained M seconds —
  i.e. an **idle/stall detector**, not a hard cap. curl's own docs/blog frame
  this explicitly: a fixed total timeout "needs to be set unnecessarily high
  to cover worst cases" for variable-size transfers, whereas the speed-based
  check "detect[s] and abort[s] stale transfers" without penalizing a transfer
  that is merely slow but still moving. curl's stated best practice is to use
  **both together** — a fast-fail connect timeout, a generous overall cap, and
  a speed/stall timeout in the middle.
- **Documented, Go ecosystem**: Go's `net/http` docs and community guidance
  converge on the same conclusion from the opposite direction — `Client.Timeout`
  is a **total** deadline that "includes the time spent reading the response
  body," which is explicitly called out as the wrong tool for large/streaming
  downloads; the recommended fix is a `context.WithTimeout`/per-read deadline
  approach instead of (or in addition to) a blanket total timeout.
- **Synthesis for `ocx`'s current 60s-total / 120s-idle split**: this maps
  cleanly onto the curl model — the index path's **60s total deadline** is the
  "fixed cap" failure mode curl's own docs warn is wrong for variable-size,
  proxy-slowed transfers (a large index document on a throttled corporate link
  can legitimately take longer than 60s while still making steady progress);
  the registry path's **120s idle/per-frame bound** is the correct shape (it
  only fires on an actually-stalled connection). The asymmetry itself — total
  deadline on the path more likely to be proxy-throttled, idle-only on the
  other — is backwards relative to the curl/Go guidance above, which favors
  idle/stall detection (with a generous outer cap as a backstop) for exactly
  the "slow-but-progressing corporate link" scenario described.

### 3e. What is folklore vs. documented — explicit summary

- **Documented**: TLS-interception doubles handshake cost (crypto literature);
  ALPN stripping forcing HTTP/1.1 by specific named enterprise proxy products
  (vendor/community bug trackers); Docker's own recommendation to lower
  concurrency on slow/proxied links; Harbor's non-coalescing cache-miss
  behavior (GitHub issue, maintainer-acknowledged); curl's dual-timeout model
  and its stated rationale.
- **Folklore / plausible but not found in a primary source**: exact connection
  ceilings per proxy vendor; GHCR's registry-specific (as opposed to
  GitHub-API-wide) rate limit numbers; whether Artifactory/Nexus coalesce
  cache-miss fetches.
- **Could not establish / needs empirical testing against the real target,
  not more literature search**: whether ocx's actual index host issues
  redirects or supports conditional GET; the specific corporate proxy's
  concurrent-connection behavior.

---

## 4. Retry semantics for idempotent GETs against a registry

### 4a. What's safe to retry

- **Idempotency**: GET is always idempotent by HTTP semantics — Google Cloud
  Storage's retry-strategy docs state this plainly (*"All get/list requests...
  "* are "always idempotent") and use it as the basis for enabling automatic
  retry by default on read paths. The same reasoning applies directly to OCI
  manifest/blob/tag-list/catalog GETs — none of them mutate registry state.
- **Status codes treated as retryable** by both AWS guidance and GCS's
  documented default policy: `408`, `429`, and the `5xx` range (`500`, `502`,
  `503`, `504`). GCS additionally retries transport-level failures: socket
  timeouts, TCP resets/refused connections, unexpected connection closures.
  ACR's own docs (§2 above) explicitly recommend retry-with-backoff on its
  `429`s. None of the sources found justify retrying `4xx` client errors other
  than `408`/`429` (e.g. a `401` should trigger a **token refresh + retry once**,
  per §1a, not a blind retry-as-is; a `403`/`404` should not be retried).
- **`Retry-After` semantics** (RFC 9110 §10.2.3, applies to `429` and `503`):
  value is *either* an integer delay-seconds *or* an HTTP-date — a client that
  assumes integer-only will mis-parse a subset of conformant responses. ACR's
  docs additionally warn the value is **dynamic across repeated polls**
  (counts down in real time, don't cache/hardcode the first value seen).

### 4b. Backoff shape and budgets — best practice, not fixed attempt counts

- **Full Jitter is the documented best-performing algorithm under contention**
  (AWS Architecture Blog, with cited simulation results): `sleep = random(0,
  min(cap, base * 2^attempt))`. AWS's own comparison found Full Jitter does
  less total client/server work than Equal Jitter (`cap/2 + random(0, cap/2)`)
  and is recommended as "a standard approach for remote clients," at the cost
  of slightly more wall-clock time to eventually succeed than more aggressive
  variants — an explicit throughput-vs-latency tradeoff, not a free win.
  Real-world registry-adjacent precedent: GCS's own default backoff uses a
  ~1s initial delay, 2.0x multiplier, 30–60s cap, with retry-limit defaults
  that vary by SDK language (3 in Node.js/PHP, up to 32 in `gcloud`, unbounded-
  but-context-timeout-gated in Go/Java) — i.e. even one vendor doesn't use one
  universal attempt count; the limit is workload-sensitivity-dependent by
  design, matching AWS Well-Architected's "control and limit retry calls"
  guidance.
- **Retry *budgets*, not fixed per-request attempt caps, are the more
  advanced/robust pattern** — Google's SRE book (`sre.google/sre-book/handling-
  overload`) documents: (1) a per-request cap (3 attempts) as a floor, but (2)
  the more load-bearing mechanism is a **per-client retry budget**: track the
  ratio of retry-traffic to total traffic, and stop retrying once that ratio
  exceeds ~10%. The book's stated rationale is retry amplification — if
  request layers each retry 3x independently, a single failure can fan out
  multiplicatively (their example: 4³ = 64x amplification through 3 layers).
  For a single-process client like `ocx` doing 512-way fan-out, the same
  amplification risk exists **within one process** if every one of 512 workers
  independently retries 3x on a shared bottleneck (e.g. the proxy itself, or
  one overloaded registry) — a global retry budget for the sync run as a whole
  is the documented mitigation, not per-request retry counts alone.

### 4c. Synthesis for the index path specifically

The index path currently has **no retry at all**, against a target (a single
timed-out request in a 512-way batch) that — per every source above — is
exactly the shape retry-with-jittered-backoff exists to solve: a transient,
idempotent-GET, proxy-adjacent failure. The two things every cited source
agrees on that a minimal retry layer must get right: (1) treat `401` as
"refresh token, retry once" separately from transport/5xx/429 retries (§1a,
§2), because token TTL can be shorter than a slow batch; and (2) budget
retries **globally across the batch**, not per-request, given the process is
already running 512-way concurrent — an uncapped per-request retry policy
independently applied 512 times is the amplification failure mode the SRE
book warns about, not a fix for it.

---

## Sources

- OCI Distribution Spec (main): https://github.com/opencontainers/distribution-spec/blob/main/spec.md
- OCI Distribution Spec extensions/reserved namespaces: https://github.com/opencontainers/distribution-spec/blob/main/extensions/README.md
- Docker/CNCF Distribution API reference (`_catalog`, non-normative today): https://distribution.github.io/distribution/spec/api/
- Docker Registry Token Authentication spec: https://distribution.github.io/distribution/spec/auth/token/ , https://docs.docker.com/reference/api/registry/auth/
- Docker Hub usage/rate limits: https://docs.docker.com/docker-hub/usage/
- GHCR rate-limit community reports: https://github.com/orgs/community/discussions/42479
- Amazon ECR throttling/common errors: https://docs.aws.amazon.com/AmazonECR/latest/userguide/common-errors.html ; quota raise: https://aws.amazon.com/about-aws/whats-new/2020/02/ecr-raises-simplifies-image-api-quotas-start-new-workloads-quicker
- Azure Container Registry SKU/rate limits: https://learn.microsoft.com/en-us/azure/container-registry/container-registry-skus
- Quay.io 429 docs: https://docs.quay.io/issues/429.html
- Harbor proxy-cache concurrent-miss issue: https://github.com/goharbor/harbor/issues/22570
- Docker daemon reference (`--max-concurrent-downloads`): https://docs.docker.com/reference/cli/dockerd/
- containerd/Moby global concurrency requests: https://github.com/moby/moby/pull/53081 , https://github.com/containerd/containerd/issues/2195
- TLS handshake CPU cost / interception doubling: https://arxiv.org/pdf/2603.11006 and general TLS performance literature (see search results)
- ALPN stripping by enterprise TLS-interception proxies: https://community.fortinet.com/fortigate-3/technical-tip-explicit-proxy-policy-evaluation-and-alpn-http-2-downgrade-failures-due-to-l7-partial-matching-228238 ; https://knowledge.broadcom.com/external/article/253017/troubleshooting-http2-traffic-in-wss.html
- curl speed-limit vs max-time: https://daniel.haxx.se/blog/2020/05/11/curl-ootw-y-speed-limit/ , https://everything.curl.dev/usingcurl/transfers/tooslow.html
- Go `net/http` Client.Timeout and large bodies: https://github.com/golang/go/issues/31657 and community guidance cited therein
- RFC 9110 §10.2.3 Retry-After: https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Retry-After
- AWS Exponential Backoff and Jitter: https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/
- AWS Well-Architected — control and limit retries: https://docs.aws.amazon.com/wellarchitected/latest/framework/rel_mitigate_interaction_failure_limit_retries.html
- Google Cloud Storage retry strategy: https://docs.cloud.google.com/storage/docs/retry-strategy
- Google SRE Book — Handling Overload: https://sre.google/sre-book/handling-overload/
