# Research: OCI registry auth/transport patterns in mature clients

Context: OCX's vendored `oci-client` fork re-runs the full `GET /v2/` challenge +
token exchange on every registry op because the public `auth()` entry point never
consults the cache it populates, and concurrent misses for the same
`(registry, repository, operation)` key are not coalesced. This surveys how
go-containerregistry, containerd, oras-go, containers/image (skopeo), and
docker/moby handle the same problem, and settles the reqwest connection-pool
question.

---

## 1. How mature clients handle challenge probing + token caching

### `google/go-containerregistry` (crane) — **the cautionary tale, not the model**

Source: [`bearer.go`](https://github.com/google/go-containerregistry/blob/main/pkg/v1/remote/transport/bearer.go)

- `bearerTransport` holds **one** `bearer authn.AuthConfig` field per transport
  instance — not a map. There is no per-scope or per-repository cache at all.
- Scopes only ever **accumulate**: on a challenge it does
  `newScopes = append(newScopes, bt.scopes...); bt.scopes = newScopes`, so a
  transport's scope set grows monotonically and is never narrowed.
- The `/v2/` challenge is read reactively off each `401` response
  (`authchallenge.ResponseChallenges(res)`), not pre-probed once — but because
  there's no cross-call token cache, in practice a fresh `remote.Image`/`remote.Get`
  call re-runs the full ping+exchange every time unless the caller manually
  threads the same `remote.WithTransport(...)` through. This is exactly OCX's bug,
  just from a different root cause (missing entry-point vs. missing cache).
- **No coalescing.** `bt.bearer` is behind a `sync.RWMutex` for read/write safety
  only; concurrent callers that miss the cache each independently call
  `bt.refresh()`.
- Confirmed by two issues, both **closed without a fix**:
  - [#1744](https://github.com/google/go-containerregistry/issues/1744) — asks for
    a `map[set<scope>]token` cache gated behind `WithScopedTokens`, partly for a
    **security** reason (a leaked cumulative token grants every scope ever seen,
    not just the current one) and partly for a **Harbor compatibility** bug
    (sending stale credentials on the ping causes Harbor to return a useless Basic
    challenge instead of Bearer). **Closed as not planned.**
  - [#740](https://github.com/google/go-containerregistry/issues/740) — a user
    listing many tags found "every `remote.Image` call is going through its own
    auth flow" (ping + token request per call), i.e. the exact multiplication OCX
    has per-layer. No maintainer fix visible.

**Takeaway:** crane is not prior art to copy — it's evidence that "cache only in
the transport, no scope map, no coalescing" is a known, unresolved pain point
even in a widely-used library. Don't converge on this design by accident.

### `containerd` — the actual reference implementation

Source: [`core/remotes/docker/authorizer.go`](https://github.com/containerd/containerd/blob/main/core/remotes/docker/authorizer.go)

- `dockerAuthorizer.handlers map[string]*authHandler` — **keyed per host**.
  `AddResponses()` short-circuits if a handler for that host already exists, so
  the `WWW-Authenticate` challenge is parsed/probed **once per host for the
  process lifetime**, not per request.
- Each `authHandler` has `scopedTokens map[string]*authResult`, keyed by
  `strings.Join(scopes, " ")` — **per-scope-set cache nested under per-host**.
  This directly answers "keyed on registry or repository": it's registry (host)
  for the challenge/handler, and scope-string (which encodes repository +
  action, e.g. `repository:foo/bar:pull`) for the token.
- **Coalesced.** `authResult` embeds `sync.WaitGroup`; the first goroutine to
  miss the cache for a scope calls `Add(1)` and fetches, every other concurrent
  caller for the *same scope key* blocks on `r.Wait()` and reads the already-filled
  `token`/`err` fields. This is exactly the missing-coalescing behavior OCX
  should add — containerd's structure is a near-literal template
  (`map[string]*authResult` + embedded `WaitGroup` per key).
- Related bug class confirmed in the wild: [containerd#8415](https://github.com/containerd/containerd/issues/8415)
  and [containerd#3556](https://github.com/containerd/containerd/issues/3556) — both
  about scope handling going wrong when a *different* repository under the same
  host needs a token the cache doesn't have, or the challenge doesn't carry a
  scope. Confirms scope-keying is the fiddly part, not host-keying.

**Recommendation: model OCX's fix on containerd's shape** — host-level
challenge/handler cache (probe once), scope-string-keyed token cache nested
under it, `WaitGroup`-per-key (or `tokio::sync::OnceCell`/`Mutex<HashMap<..,
Shared<...>>>` in Rust) for coalescing.

### `oras-go` v2

Source: [`registry/remote/auth/client.go`](https://github.com/oras-project/oras-go/blob/main/registry/remote/auth/client.go), [`auth` package docs](https://pkg.go.dev/oras.land/oras-go/v2/registry/remote/auth)

- `Client.Cache` is an explicit interface (`nil` = no caching, opt-in
  `auth.NewCache()` for a "go-routine safe" implementation). `DefaultClient` wires
  in `DefaultCache` by default.
- Cache key is **`(host, scheme, key)`** where `key` is `""` for Basic and the
  joined scope string for Bearer — same host+scope-tuple shape as containerd,
  just flattened into one cache call: `cache.Set(ctx, host, SchemeBearer,
  strings.Join(scopes, " "), fetchFn)`.
- **Coalescing is delegated to the `Cache` implementation** via a `Set(ctx, ...,
  fetch func(ctx) (string, error))` signature — the interface itself is shaped so
  a correct implementation *can* single-flight, but the client code doesn't force
  it. Worth checking oras-go's own `NewCache()` body if reusing this exact shape;
  the interface contract is the useful part regardless.
- Challenge handling: cached token is tried first (no request wasted probing);
  `WWW-Authenticate` is only parsed on an actual `401`, and a scope miss there
  triggers exactly one fetch+cache-set, not a blanket re-probe.

### `containers/image` (skopeo, podman) — a real regression to avoid

- [containers/image#2754](https://github.com/containers/image/issues/2754): the
  `docker` transport pings `/v2/` and, if that returns `200`, **assumes the whole
  registry is unauthenticated** and never revisits `WWW-Authenticate` on
  subsequent per-repository requests — even when a `401` comes back from a
  specific repo endpoint. This is the opposite failure mode from crane/containerd:
  caching *too eagerly* at the wrong granularity (whole registry vs. per-repo)
  breaks multi-tenant / mixed-ACL registries. Confirms: **cache the challenge
  per-host is fine only if you still honor a later 401 from a specific
  repository** — don't let a registry-wide "no-auth-needed" conclusion suppress
  a legitimate later challenge.
- [containers/image PR #669](https://github.com/containers/image/pull/669) — uses
  the same `http.Client` for the token-service request and the registry request,
  a small but real detail (reuses the connection pool / proxy config / TLS
  settings instead of a second ad hoc client).

### `docker`/`moby` and `distribution/distribution`

`docker/cli` itself doesn't run pulls — pulls go through `moby/moby` (dockerd) or
`buildkit`, both of which vendor `distribution/distribution`'s
`registry/client/auth`, whose `tokenHandler` follows the same lineage as
containerd's authorizer (they share history/authorship — Aaron Lehmann's
[moby#20832](https://github.com/moby/moby/pull/20832) explicitly ports
"token handling code from distribution" into docker's login path). I could not
pull the current godoc for `distribution/distribution/v3/registry/client/auth`
(404 on pkg.go.dev — likely an import-path/version mismatch) to quote its cache
struct directly; treat containerd's `authorizer.go` as the representative
implementation for this whole family rather than re-deriving distribution's copy.
One real-world scope-cache failure mode is documented in
[buildkit#5883](https://github.com/moby/buildkit/issues/5883): a token scoped to
one repository, cached, then reused/misapplied when a different repo under the
same build needs different permissions → `insufficient_scope`. **Lesson for
OCX:** cache key must include the full scope (repo + verb set), never just the
registry host, or you'll serve a stale/under-scoped token silently.

---

## 2. Is per-request re-auth a recognized anti-pattern?

Yes, unambiguously, across every project surveyed:

- go-containerregistry [#740](https://github.com/google/go-containerregistry/issues/740) —
  named exactly OCX's symptom ("every call going through its own auth flow"),
  filed as a real user complaint about tag-listing performance.
- go-containerregistry [#1744](https://github.com/google/go-containerregistry/issues/1744) —
  the *lack* of scope-keyed caching is flagged as both a perf and a security
  problem (over-broad cached credential).
- containers/image [#2754](https://github.com/containers/image/issues/2754) shows
  the inverse failure (over-aggressive caching) is equally real — the anti-pattern
  isn't just "cache nothing", it's "cache at the wrong granularity" in either
  direction.
- containerd's own design (host+scope cache, coalesced) is the field's converged
  answer, and it predates all of the above complaints — i.e., the "right" shape
  was known before crane and containers/image shipped their weaker versions.

I could not find a dedicated blog post/RFC specifically about Docker Hub rate
limits being *caused* by redundant token exchanges (search returned only generic
"how to avoid Docker Hub rate limits" guides — authenticate, cache layers,
pull-through cache). **Flagging this as unestablished**: I found strong evidence
re-auth-per-request is wasteful and disliked, but no citation quantifying its
Docker-Hub-rate-limit cost specifically. Token-exchange requests hit the
*auth realm* (e.g. `auth.docker.io`), not the pull-count-limited registry
endpoint itself, so they likely don't consume Docker Hub's pull quota directly —
but they do consume the *auth service's* own (undocumented) rate limits, add
latency per operation, and multiply badly at OCX's stated concurrency (up to 512
in flight). Treat the cost as "many wasted round-trips + latency", not
"quota exhaustion", absent a citation for the latter.

---

## 3. Token scope caching subtleties

- **Different repository ⇒ new token, not a multi-scope token, in every client
  surveyed.** None of go-containerregistry, containerd, oras-go, or
  containers/image request a token covering multiple repositories up front, and
  none merge scopes speculatively except crane's already-flagged anti-pattern of
  *accumulating* scopes onto one token per transport instance (which #1744 wants
  removed). The token spec does allow a request to carry multiple `scope`
  params (seen in the containerd search hits, e.g. two `scope=repository:...`
  values in one token request when a mirror needs to reference both an upstream
  and local repo name) but that's the *request* asking for a union up front
  when the caller already knows both scopes are needed together — it is not
  proactive speculative widening.
- **Cache key = host + scope-string is the converged pattern** (containerd,
  oras-go). Keying on registry alone is wrong (buildkit#5883-style
  under-scoping bugs); keying on repository alone is insufficient because a
  push+pull token differs in scope from a pull-only token for the same repo —
  scope string must include the verb set, not just the repo name.
- **Opaque non-JWT tokens (e.g. GHCR's):** none of the surveyed clients parse
  the token to determine expiry when it's opaque — they rely on the token
  response's own `expires_in`/`issued_at` fields (part of the [distribution
  token spec](https://distribution.github.io/distribution/spec/auth/token/)),
  defaulting to a fixed TTL (commonly 60s per the spec's guidance) when absent.
  containerd's `authResult` stores `expirationTime *time.Time` computed from the
  response at fetch time, not from decoding the token — this is the safe,
  registry-agnostic approach and works identically whether the token is a JWT or
  an opaque string. **Do not assume JWT and decode `exp`** — treat the token as
  opaque and trust the token endpoint's stated lifetime, falling back to a
  conservative default (containerd/skopeo-style clients commonly treat a missing
  `expires_in` as ~60s) if the field is absent.

---

## 4. Corporate proxy behavior (load-bearing question)

### reqwest's actual current defaults — resolved

Authoritative, from [`docs.rs/reqwest` `ClientBuilder`](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html):

- **`pool_idle_timeout`: default `90` seconds** (`Some(Duration::from_secs(90))`).
- **`pool_max_idle_per_host`: default `usize::MAX` — i.e. unlimited idle
  connections kept per host.** (Some web summaries claim a default of `1`; that
  is wrong for current reqwest — verified directly against the docs.rs page
  text, which says "Default is no limit.")

Implication for OCX: reqwest will *not* artificially cap concurrent connections
to a registry host by itself. Under HTTP/1.1 (see below), up to N concurrent
requests to the same host will open up to N TCP+TLS connections (bounded only by
your own concurrency limit — OCX's stated 512 in flight — and by
`pool_idle_timeout` deciding how long those connections are kept warm
afterward, not how many can exist concurrently). There is no separate
"max total connections" knob in reqwest beyond `pool_max_idle_per_host`
(idle pool sizing) — concurrent in-flight connections are effectively unbounded
client-side.

### HTTP/2 survival through TLS-intercepting corporate proxies — does NOT survive by default

This is the closest thing to a documented, cross-vendor consensus I found, and it
points the same direction from every proxy vendor searched:

- **Zscaler**: "Non-browser requests for HTTP/2 connections will be downgraded to
  HTTP/1.1 connections by Zscaler service edges if the non-browser HTTP/2 traffic
  configuration is kept disabled" — i.e. off by default for exactly the kind of
  client OCX is (a CLI, not a browser). [Zscaler community](https://community.zscaler.com/s/question/0D5PJ00000dRd5m0AC/enabling-http2), [Zscaler blog](https://www.zscaler.com/blogs/product-insights/overcome-http-2-complexities-zscaler).
- **Juniper (SSL Proxy / Junos)**: "When HTTP/2 is turned off... the proxy removes
  h2 from the list of protocols before connecting to the server, resulting in the
  server negotiating http/1.1 instead of HTTP/2." — [Juniper docs](https://www.juniper.net/documentation/us/en/software/junos/application-identification/topics/topic-map/http2-inspection-for-ssl-proxy.html).
- **Palo Alto Networks**: HTTP/2 inspection requires the firewall to actively
  participate in both legs of the TLS handshake and ALPN; when it can't/won't,
  it downgrades or classifies the traffic as unknown TCP. [Palo Alto docs](https://docs.paloaltonetworks.com/ngfw/administration/app-id/http2), [LIVEcommunity](https://live.paloaltonetworks.com/t5/community-blogs/http-2-inspection/ba-p/337392).
- **Broadcom/Symantec Cloud SWG**, **Fortinet FortiGate**: same pattern —
  HTTP/2 inspection is a distinct, often-off-by-default feature; without it the
  proxy strips or fails ALPN and the connection negotiates HTTP/1.1.
  [Broadcom](https://knowledge.broadcom.com/external/article/253017/troubleshooting-http2-traffic-in-wss.html), [Fortinet](https://docs.fortinet.com/document/fortigate/7.0.0/new-features/710924/http-2-support-in-proxy-mode-ssl-inspection).
- **mitmproxy issue [#8192](https://github.com/mitmproxy/mitmproxy/issues/8192)**
  documents the inverse bug (proxy forces h2 ALPN with the client while only
  supporting HTTP/1.1 CONNECT), which underscores how fragile ALPN negotiation
  is across any TLS-terminating intermediary — implementations disagree even on
  which side gets which protocol.

**Conclusion for OCX's design**: assume any client running behind a
TLS-inspecting corporate proxy (Zscaler, Netskope, and enterprise NGFWs
generally) gets **HTTP/1.1, not HTTP/2**, to the registry, unless that org has
explicitly turned on HTTP/2 inspection (uncommon — it's extra CPU cost for the
proxy vendor and is frequently left off). Combined with reqwest's unbounded
`pool_max_idle_per_host`, a burst of up to 512 concurrent OCX requests behind
such a proxy means **up to 512 concurrent CONNECT tunnels / TCP connections**,
each doing its own TLS handshake — not 512 streams multiplexed over a handful of
h2 connections. That is a real, citable operational risk (proxy CONNECT-rate
limiting, ephemeral port exhaustion, proxy-side connection caps) independent of
the token-exchange redundancy problem, and it argues for **client-side
concurrency limiting/backpressure being load-bearing, not optional**, when
behind a corporate proxy — reqwest/hyper will not protect you from yourself
here.

I could not find an Artifactory-specific citation on this (searches returned
only generic pull-through-cache setup guides, nothing on its HTTP/2 posture) —
flagging as unestablished rather than guessing; the general TLS-interception
behavior above should be assumed to apply equally to it since Artifactory as a
pull-through cache is typically deployed *behind* the same corporate egress
proxy/firewall as everything else, not instead of one.

---

## 5. Retry + backoff norms

| Client | Default retried codes | Backoff | Notes |
|---|---|---|---|
| **oras-go** `retry` pkg ([godoc](https://pkg.go.dev/oras.land/oras-go/v2/registry/remote/retry)) | 5xx, 429, 408, network dial timeout | Exponential, base 250ms, factor 2, **10% jitter**; interval formula `temp*(1-jitter) + rand(2*jitter*temp)`; MinWait 200ms, MaxWait 3s, MaxRetry 5 | 429 included by default — the "right" answer |
| **go-containerregistry** `transport`/`remote` (crane) | `defaultRetryStatusCodes` = 408, 500, 502, 503, 504, 499, 522 — **429 excluded by default** | `WithRetryBackoff`, configurable; not fetched in detail here | Confirmed bug: [#2111](https://github.com/google/go-containerregistry/issues/2111) — 429 is classified internally as a temporary error but the retry transport's status-code allowlist doesn't include it, so it's silently never retried unless the caller passes `WithRetryStatusCodes` explicitly. Closed via PR #2301 (fix status not independently confirmed here — verify current source before relying on "it's fixed") |
| **containerd** | not directly sourced in this pass | — | out of scope for this table; containerd's retry logic lives in its transport/resolver, not the authorizer file reviewed above |

**Jitter at 512-way concurrency: yes, necessary, not optional.** oras-go's own
default already bakes in 10% jitter at far lower expected concurrency than
OCX's stated ceiling; without jitter, a shared rate-limit or transient 503 hit
by many of 512 in-flight requests simultaneously will retry in lockstep and
reproduce the same spike. This is standard distributed-systems guidance
(thundering-herd avoidance), not something unique to registries, but the
concrete oras-go numbers (250ms base ×2 factor ±10%, capped 3s, 5 attempts) are
a reasonable, field-tested starting point to adapt rather than deriving retry
tuning from scratch.

**429 handling is the one place to explicitly diverge from crane's default and
match oras-go**: treat 429 as retryable by default, and honor `Retry-After` when
present (RFC 9110) before falling back to computed backoff — none of the Go
clients surveyed were shown honoring `Retry-After` explicitly in the snippets
retrieved here, so if OCX adds that it would be ahead of at least crane's
documented default, not just catching up.

---

## Summary / recommendations

1. **Model the cache on containerd's `authorizer.go` shape**: per-host challenge/handler
   cache (probe `/v2/` once per host per process) + per-scope-string token cache
   nested under it + `WaitGroup`-per-key coalescing. This is the most-converged,
   least-buggy design across everything surveyed — oras-go's `(host, scheme,
   scope)` cache key is the same shape, just Go-interface-shaped differently.
2. **Don't copy crane's model** (single token, no scope keying, no coalescing) —
   it's the club's own well-documented anti-pattern, still unfixed
   ([#1744](https://github.com/google/go-containerregistry/issues/1744),
   [#740](https://github.com/google/go-containerregistry/issues/740)).
3. **Don't copy skopeo's registry-wide "no auth needed" latch either**
   ([#2754](https://github.com/containers/image/issues/2754)) — cache the
   challenge per host, but still honor a later 401 from a specific repository;
   don't let one green ping suppress all future challenges.
4. **Cache key must be the full scope string (repo + verb set), never registry
   alone** — buildkit's insufficient_scope bugs
   ([#5883](https://github.com/moby/buildkit/issues/5883)) are what
   under-scoping looks like in production.
5. **Treat tokens as opaque; trust the token response's stated
   expiry/`expires_in`, don't decode JWTs to find `exp`** — this is
   registry-agnostic and matches containerd's approach, and specifically
   required for GHCR's non-JWT tokens.
6. **The corporate-proxy risk is real and is HTTP/1.1-fallback + unbounded
   reqwest connection pooling, not H2-multiplexing-survives-fine** — cross-vendor
   proxy docs (Zscaler, Juniper, Palo Alto, Fortinet, Broadcom) agree non-browser
   HTTP/2 is commonly downgraded under TLS inspection. Combined with reqwest's
   confirmed-from-docs `pool_max_idle_per_host` default of unlimited, OCX's own
   concurrency limiter is the only thing standing between "512 in flight" and
   "512 concurrent CONNECT tunnels" — size and test that limiter with a
   corporate-proxy scenario in mind, don't rely on the HTTP layer to self-limit.
7. **Adopt oras-go's retry defaults as a starting point** (5xx+429+408, 250ms/×2/10%
   jitter, capped ~3s, ~5 attempts) rather than crane's (429 excluded by
   default — a confirmed, if since-patched, footgun). Honor `Retry-After` when
   present.

## Unresolved / flagged as unestablished
- No citation found quantifying Docker Hub rate-limit cost specifically
  attributable to redundant token exchanges (vs. general pull-count limits).
- Could not fetch `distribution/distribution`'s current `registry/client/auth`
  source directly (404 on the import path tried); relied on containerd's fork of
  the same lineage as the representative implementation instead.
- No Artifactory-specific citation on HTTP/2 posture as a pull-through cache;
  assumed to inherit whatever corporate egress proxy sits in front of it.
- containerd's own retry/backoff policy (as opposed to its authorizer's caching,
  which is well-sourced above) was not pulled from source in this pass.
