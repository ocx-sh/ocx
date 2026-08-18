# Research: self-hosted Sigstore stack for acceptance tests + docs

Axis: running a real Sigstore stack locally, in docker-compose, for `test/docker-compose.yml`
and for self-hosting documentation. All facts below are from primary sources (upstream repos,
fetched via `gh api`/`gh search`, August 2026), not from training-data recall — several
project shapes changed materially in 2025–2026 and stale recall would be actively wrong here.

**Headline correction to the mission brief's assumption**: the brief describes CTFE as
"certificate-transparency-go". That is stale. As of August 2026, `sigstore/fulcio`'s own
`docker-compose.yml` runs a service literally named `tesseract`, built from
`Dockerfile.tesseract`, which is **TesseraCT** (`github.com/transparency-dev/tesseract`) — a
Trillian-Tessera-based CT log, not `google/certificate-transparency-go`'s `ctfe` binary. See
§1 and §2.

---

## 1. What are the deployable components, and which are strictly required

A real Sigstore keyless-signing round trip (`cosign sign` / `sigstore-rs` `SigningSession`)
needs, at minimum:

| Component | Required for `sign`? | Required for `verify`? | Notes |
|---|---|---|---|
| **Fulcio** (CA, issues short-lived code-signing certs) | Yes | No (verify only needs Fulcio's *root cert*, not a live server) | gRPC :5554 + HTTP :5555 in upstream compose |
| **An OIDC issuer Fulcio trusts** | Yes | No | Dex with a no-interaction connector, or a CI provider's ambient OIDC |
| **CT log (Fulcio embeds an SCT in every cert)** | Yes — Fulcio's own `fileca` CA path calls out to `--ct-log-url` synchronously during cert issuance | Only if the client verifies SCTs | Now **TesseraCT**, not `ctfe`/`certificate-transparency-go`. Confirmed live in `sigstore/fulcio/docker-compose.yml` (`tesseract` service, `Dockerfile.tesseract`) |
| **Rekor** (transparency log for the signature) | Yes, if the client uploads a Rekor entry (`cosign sign` / `sigstore-rs` do by default) | Yes, if the client verifies inclusion | Two live shapes — see §2. "Classic" Rekor v1 needs Trillian + a SQL DB; **Rekor v2 (`rekor-tiles`) needs neither** |
| **Trillian log server + log signer** | Only for classic Rekor v1, and only as Rekor v1's backing store | — | Not used by Rekor v2 at all (tile-based instead) |
| **MySQL/MariaDB** | Only for classic Rekor v1 (Trillian storage) and Rekor v1's search index | — | Rekor v2 POSIX backend needs **no database** |
| **Redis** | Only for classic Rekor v1's search-by-email/hash index (`rekor-server.yaml` `search_index`) | — | Not present in Rekor v2 at all |
| **TSA (Timestamp Authority)** | No — optional, used for long-lived verification independent of Rekor | No, unless the bundle carries a timestamp | Skip for a minimal stack; `cosign trusted-root create --no-default-tsa` |
| **A `trusted_root.json` / TUF root** | No (signing doesn't need it) | Yes — the verifier's pinned root of trust | Generated once, offline, from the running stack's own keys — see §4 |

**Answer to "which can be omitted"**: for a minimal-but-real stack that makes `cosign sign`
and a `sigstore-rs` client succeed, you need Fulcio + a CT log (Fulcio hard-requires
`--ct-log-url` on its `fileca` CA path — there is no flag to skip SCT embedding) + an OIDC
issuer + Rekor. TSA is skippable. Trillian/MySQL/Redis are skippable **if you pick Rekor v2
(rekor-tiles, POSIX backend)** instead of classic Rekor v1 — this is the single biggest
simplification available and is new since the mission brief's "Trillian + MySQL" framing.

Sources: [sigstore/fulcio docker-compose.yml](https://github.com/sigstore/fulcio/blob/main/docker-compose.yml), [sigstore/rekor docker-compose.yml](https://github.com/sigstore/rekor/blob/main/docker-compose.yml), [sigstore/rekor-tiles README](https://github.com/sigstore/rekor-tiles/blob/main/README.md)

---

## 2. Concrete compose recipes that already exist upstream

### 2a. `sigstore/fulcio/docker-compose.yml` (fetched in full, 2026-08)

```yaml
services:
  fulcio-server:
    build:
      context: .
      target: "deploy"
    command: [
      "fulcio-server", "serve",
      "--host=0.0.0.0", "--port=5555", "--grpc-port=5554",
      "--ca=fileca",
      "--fileca-cert=/etc/fulcio/root.pem",
      "--fileca-key=/etc/fulcio/root.key",
      "--fileca-key-passwd=fulcio",
      "--ct-log-url=http://tesseract:6962",
    ]
    restart: always
    ports: ["5555:5555", "5554:5554", "${FULCIO_METRICS_PORT:-2112}:2112"]
    volumes:
      - ~/.config/gcloud:/root/.config/gcloud/:z   # only for GCP KMS auth, irrelevant to fileca
      - ${FULCIO_CONFIG:-./config/identity/config.yaml}:/etc/fulcio-config/config.yaml:z
      - ./config/fulcio-root:/etc/fulcio:ro
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:5555/healthz"]
      interval: 10s
      timeout: 3s
      retries: 5
      start_period: 30s
    depends_on:
      dex-idp:
        condition: service_healthy
    read_only: true

  dex-idp:
    build:
      context: .
      dockerfile: Dockerfile.dex-idp   # FROM dexidp/dex:v2.45.1@sha256:8499afd6...
    user: root
    command: ["dex", "serve", "/etc/config/docker-compose-config.yaml"]
    restart: always
    ports: ["8888:8888"]
    healthcheck:
      test: ["CMD", "wget", "-O", "/dev/null", "http://localhost:8888/auth/healthz"]
      interval: 10s
      timeout: 3s
      retries: 3
      start_period: 5s

  tesseract:                          # <- NOT certificate-transparency-go
    build:
      context: .
      dockerfile: Dockerfile.tesseract
    volumes:
      - ctStorage:/ctfe:z
      - ./config/ctfe/:/etc/ctfe:ro
      - ./config/fulcio-root:/etc/fulcio:ro
    user: root
    command: [
      "--private_key", "/etc/ctfe/privkey.pem",
      "--origin", "tesseract",
      "--storage_dir", "/ctfe",
      "--roots_pem_file", "/etc/fulcio/root.pem",
      "--v", "1",
      "--ext_key_usages", "CodeSigning",
      "--http_endpoint", "0.0.0.0:6962",
    ]
    healthcheck:
      test: ["CMD", "curl", "-f", "-k", "http://localhost:6962/healthz"]
      interval: 5s
      timeout: 3s
      retries: 15
      start_period: 15s
    restart: always
    ports: ["6962:6962"]

  ct-read:                            # serves the CT log's static tile storage over HTTP
    image: nginx:1.31.3@sha256:5a88c9c45479443d7be2eadc894b4ed0a9801bae03d97a5760ae13b5c2005942
    volumes: ["ctStorage:/usr/share/nginx/html"]
    user: root
    restart: always
    ports: ["8000:80"]

volumes:
  ctStorage: {}
```

Everything here is `build: context: .` (source build from the fulcio repo checkout) except
`dex-idp` (built from a tiny wrapper `Dockerfile.dex-idp` around the published
`dexidp/dex:v2.45.1` image) and `ct-read`/`nginx`. **There is no premade `ghcr.io/sigstore/fulcio-server:latest` tag wired into this compose file** — but `ghcr.io/sigstore/fulcio` and `ghcr.io/sigstore/helm-charts/ctlog` (the older CTFE) **and** `ghcr.io/sigstore/helm-charts/ctlog-tiles` (TesseraCT-based) images do exist in the `sigstore` GHCR org (confirmed via package listing), used by the Helm charts. For a compose-only setup, either build from source (matches upstream exactly, slower first run) or pull `ghcr.io/sigstore/fulcio:<tag>` + a tesseract image directly and pass equivalent flags.

**No Fulcio-published multi-arch guarantee was found in the fetched files** — mark
UNCONFIRMED for `linux/arm64`; the fulcio/rekor container images are built via `ko`
(`.ko.yaml` present in both repos), and `ko` defaults to multi-platform builds unless
restricted, but the actual published manifest list wasn't independently inspected. Since ocx CI likely runs `ubuntu-latest` (amd64), this is not a blocker either way.

### 2b. `sigstore/rekor/docker-compose.yml` — classic Rekor v1 (Trillian-backed, heavy)

```yaml
services:
  mysql:
    platform: linux/amd64
    image: gcr.io/trillian-opensource-ci/db_server:v1.4.0
    environment:
      - MYSQL_ROOT_PASSWORD=zaphod
      - MYSQL_DATABASE=test
      - MYSQL_USER=test
      - MYSQL_PASSWORD=zaphod
    healthcheck:
      test: "mysqladmin -h 127.0.0.1 --user=$$MYSQL_USER --password=$$MYSQL_ROOT_PASSWORD -s ping"
      interval: 5s
      timeout: 3s
      retries: 15
      start_period: 90s

  redis-server:
    image: docker.io/valkey/valkey:9.1.1-alpine3.24
    command: ["--bind", "0.0.0.0", "--appendonly", "yes", "--requirepass", "test"]
    healthcheck: {test: ["CMD", "valkey-cli", "-a", "test", "ping"], interval: 10s, timeout: 3s, retries: 3, start_period: 5s}

  trillian-log-server:
    build: {context: ., dockerfile: Dockerfile.trillian-log-server}
    command: ["--quota_system=noop", "--storage_system=mysql",
      "--mysql_uri=test:zaphod@tcp(mysql:3306)/test?parseTime=true&interpolateParams=true",
      "--rpc_endpoint=0.0.0.0:8090", "--http_endpoint=0.0.0.0:8091", "--alsologtostderr"]
    depends_on: {mysql: {condition: service_healthy}}
    healthcheck: {test: ["CMD", "curl", "-f", "http://localhost:8091/healthz"], interval: 5s, timeout: 3s, retries: 15, start_period: 15s}

  trillian-log-signer:
    build: {context: ., dockerfile: Dockerfile.trillian-log-signer}
    command: ["--quota_system=noop", "--storage_system=mysql",
      "--mysql_uri=test:zaphod@tcp(mysql:3306)/test?parseTime=true&interpolateParams=true",
      "--rpc_endpoint=0.0.0.0:8090", "--http_endpoint=0.0.0.0:8091", "--force_master", "--alsologtostderr"]
    ports: ["8092:8091"]
    depends_on: {mysql: {condition: service_healthy}}

  rekor-server:
    build: {context: ., target: "deploy"}
    environment: ["TMPDIR=/var/run/attestations"]
    command: ["rekor-server", "serve",
      "--trillian_log_server.address=trillian-log-server", "--trillian_log_server.port=8090",
      "--rekor_server.address=0.0.0.0", "--rekor_server.signer=memory",
      "--enable_attestation_storage", "--attestation_storage_bucket=file:///var/run/attestations",
      "--search_index.storage_provider=mysql",
      "--search_index.mysql.dsn=test:zaphod@tcp(mysql:3306)/test?parseTime=true&interpolateParams=true",
      "--enable_pprof"]
    ports: ["3000:3000", "2112:2112", "6060:6060"]
    depends_on: {mysql: {condition: service_healthy}, trillian-log-server: {condition: service_healthy}}
    healthcheck: {test: ["CMD", "curl", "-f", "http://localhost:3000/ping"], interval: 10s, timeout: 3s, retries: 15, start_period: 30s}
```

`--rekor_server.signer=memory` is important: it generates an ephemeral signing key at
container start, so the Rekor public key needed for `cosign trusted-root create --rekor`
must be **scraped from the running container**, not hardcoded (see §4).

### 2c. `sigstore/rekor-tiles/posix-compose.yml` — **Rekor v2, the recommended shape (light, no DB, no Trillian)**

This is the file to actually copy for `test/docker-compose.yml`, given issue #107 targets
Rekor v2. Fetched in full:

```yaml
services:
  rekor:
    build:
      context: .
      target: deploy
      args: {STORAGE_BACKEND: posix}
    command:
      - "rekor-server"
      - "serve"
      - "--http-address=0.0.0.0"
      - "--grpc-address=0.0.0.0"
      - "--hostname=rekor-local"
      - "--storage-dir=/tmp/posixlog"
      - "--signer-filepath=/pki/ed25519-priv-key.pem"
      - "--checkpoint-interval=2s"
      - "--log-level=debug"
      - "--request-response-logging=true"
      - "--persistent-antispam=true"
      - "--witness-policy-path=/witness/policy.yaml"
    ports: ["3003:3000", "3001:3001", "2114:2112"]
    healthcheck:
      test: ["CMD-SHELL", "curl http://localhost:3000/healthz | grep '{\"status\":\"SERVING\"}'"]
      timeout: 30s
      retries: 10
      interval: 3s
    volumes:
      - ./tests/testdata/pki:/pki
      - ./tests/testdata/witness:/witness
      - posix_storage:/tmp/posixlog
    depends_on:
      nginx: {condition: service_healthy}
      witness: {condition: service_healthy}

  nginx:
    image: nginx:1.31.3@sha256:5a88c9c45479443d7be2eadc894b4ed0a9801bae03d97a5760ae13b5c2005942
    volumes: [posix_storage:/usr/share/nginx/html]
    ports: ["8000:80"]
    healthcheck: {test: ["CMD", "curl", "--fail", "http://localhost/"], interval: 30s, retries: 10, timeout: 3s}

  witness:                            # cosigns checkpoints; NOT strictly needed for a test client to succeed
    build:
      context: https://github.com/transparency-dev/witness.git#main
      dockerfile: cmd/omniwitness/Dockerfile
    volumes: [witness_data:/witness_data:rw, ./tests/testdata/witness:/witness_config]
    command: ["--listen=:8100", "--db_file=/witness_data/witness.sqlite",
      "--private_key_path=/witness_config/private.key",
      "--additional_logs=/witness_config/config.yaml", "--logtostderr", "--v=2", "--rate_limit=100"]
    healthcheck: {test: ["CMD-SHELL", "wget --spider --tries=1 http://localhost:8081/metrics || exit 1"], timeout: 30s, retries: 10, interval: 3s}
    ports: ["8100:8100"]

volumes: {posix_storage: {}, witness_data: {}}
```

Signer key is a **checked-in test fixture** (`tests/testdata/pki/ed25519-priv-key.pem`), not
generated at runtime — a much better fit for "sign once, reuse fixtures" (§5) than classic
Rekor v1's `signer=memory`. Prebuilt images exist: `ghcr.io/sigstore/rekor-tiles/posix`.

**Important caveat, confirmed from `rekor-tiles/README.md`**: "As of October 2025, we have
not yet distributed the current Rekor v2 URL in the [public] SigningConfig" — i.e. **Rekor v2
is not yet the public default**, `sigstore-rs`/`cosign` default flows still talk to Rekor v1
publicly. A self-hosted Rekor v2 stack is real and working, but a client must be pointed at
it explicitly (custom `SigningConfig`/`TrustedRoot`, not the public TUF root) — this is a
material integration detail for #107 ("Rekor v2 delta"), not just a deployment swap.

### 2d. `sigstore/scaffolding` — kind-only, no non-kind path

`getting-started.md` (fetched in full) is unambiguous: "Running locally on KinD... You
should be able to install KinD and Knative bits by running `./hack/setup-kind.sh`". There is
**no bare-docker-compose or non-Kubernetes path** documented anywhere in the repo tree
(`config/`, `hack/`, `testdata/` are all kind/Knative-shaped). Scaffolding deploys 5
namespaces (`ctlog-system`, `fulcio-system`, `rekor-system`, `tuf-system`, plus a `default`
namespace `gettoken` mock-OIDC Knative service). Confirmed via `sigstore-go`'s own e2e CI
(`sigstore/scaffolding/actions/setup-sigstore-env@main`), which needs an explicit
multi-step **disk-cleanup** action (`rm -rf` on Android SDK, .NET, Swift, GraalVM, etc.)
before it can even start kind — a strong, concrete signal that scaffolding is too heavy for a
per-`task test` invocation. **Verdict: reject scaffolding/kind for ocx; use direct
docker-compose (2a + 2c) instead.**

### 2e. `sigstore-conformance` — hits real public infra, not a local stack

`action.yml`/`setup/setup.bash` (fetched in full) show `sigstore-conformance` runs a client
against `production` (default) or `staging` **public** Sigstore infrastructure
(`fulcio.sigstore.dev`/`sigstore-staging`), authenticating via GitHub Actions' own ambient
OIDC token. It does **not** run Fulcio/Rekor itself. `gitsign`/`policy-controller` were not
independently checked (UNCONFIRMED) but the pattern across every client repo surveyed (§6) is
the same: no client repo runs its own compose-based Fulcio+Rekor stack in CI except the
`sigstore/fulcio` and `sigstore/rekor` repos' own dev/integration-test loops.

Sources: [fulcio docker-compose.yml](https://github.com/sigstore/fulcio/blob/main/docker-compose.yml), [fulcio Dockerfile.dex-idp](https://github.com/sigstore/fulcio/blob/main/Dockerfile.dex-idp), [rekor docker-compose.yml](https://github.com/sigstore/rekor/blob/main/docker-compose.yml), [rekor-tiles posix-compose.yml](https://github.com/sigstore/rekor-tiles/blob/main/posix-compose.yml), [rekor-tiles README](https://github.com/sigstore/rekor-tiles/blob/main/README.md), [scaffolding getting-started.md](https://github.com/sigstore/scaffolding/blob/main/getting-started.md), [sigstore-go e2e.yml](https://github.com/sigstore/sigstore-go/blob/main/.github/workflows/e2e.yml), [sigstore-conformance action.yml](https://github.com/sigstore/sigstore-conformance/blob/main/action.yml)

---

## 3. The OIDC issuer

**Confirmed answer: Dex, with the `mockCallback` connector, is the exact non-interactive
mechanism upstream Fulcio's own compose file uses.** Fetched in full,
`sigstore/fulcio/config/dex/docker-compose-config.yaml`:

```yaml
issuer: http://dex-idp:8888/auth

storage:
  type: memory

web:
  http: 0.0.0.0:8888

frontend:
  issuer: Fulcio in Docker Compose

expiry:
  signingKeys: "24h"
  idTokens: "1m"
  authRequests: "24h"

oauth2:
  responseTypes: ["code"]
  alwaysShowLoginScreen: true
  skipApprovalScreen: true

connectors:
  - type: mockCallback              # <- the non-interactive answer
    id: https://any.valid.url/
    name: AlwaysApprovesOIDCProvider

staticClients:
  - id: fulcio
    public: true
    name: 'Fulcio in Docker Compose'

redirectURI: http://dex-idp:8888/auth/callback
```

`mockCallback` is a built-in dex connector type that **always approves** with no user
interaction and no credentials — it exists specifically for this use case (dex's own
`connector/mock` package). This is the mechanism, not a purpose-built mock issuer nor GitHub
Actions' real OIDC (which only works *inside* GHA runs, not for local `task test`).

**How a test obtains a token non-interactively**: dex's OAuth2 device/auth-code flow still
needs *something* to drive the browser redirect even with `mockCallback` approving instantly.
Two established patterns:
1. **`sigstore-rs`'s own OIDC test helper** — `sigstore-rs` already depends on an OIDC flow
   for its own tests; check `oauthflow.rs`/interactive-login tests in the vendored crate for
   whether it supports a direct token fetch against a `mockCallback`-backed issuer
   (UNCONFIRMED — not independently verified in this pass, flag for the implementing agent).
2. **Scaffolding's `getoidctoken` pattern** — `sigstore/scaffolding/tools/getoidctoken`
   fetches a Kubernetes projected `serviceAccountToken` and writes it to
   `/var/run/sigstore/cosign/oidc-token`, then `cosign sign --identity-token=$(cat ...)`
   consumes it directly, entirely bypassing the browser flow. The k8s-specific part
   (projected service account token) doesn't transplant to docker-compose, but the **pattern**
   — obtain a bearer JWT some other way, hand it to the client via `--identity-token`/
   equivalent flag rather than driving an interactive OAuth2 flow — is exactly right and is
   what `sigstore-rs`'s test suite should do: mint a JWT directly against dex's
   `/token` endpoint using the OAuth2 **password grant is not standard here**; the
   established alternative in dex-fronted test rigs is to call the `mockCallback` connector's
   `/auth/callback` directly with a scripted `curl` walking the authorization-code flow (dex
   supports `grant-type=authorization_code` non-interactively once the mock connector
   short-circuits consent). **This exact curl sequence was not found upstream in the fetched
   files — treat as an implementation task, not a solved recipe.**

**Fulcio's `OIDCIssuers` config** (`config/identity/config.yaml`, fetched in full — this is
the *production* config listing every trusted real-world issuer, ~400 lines). The schema per
issuer (relevant excerpt, `google` entry as the shape reference):

```yaml
oidc-issuers:
  https://accounts.google.com:
    issuer-url: https://accounts.google.com
    client-id: sigstore
    type: email                      # -> claim used as SAN is the verified `email` claim
    contact: tac@sigstore.dev
    description: "Google OIDC auth"
```

`type` is the field that decides what becomes the certificate SAN: `email` → the OIDC
`email` claim (with `email_verified: true` required); `spiffe` → the `sub` claim as a SPIFFE
URI; `username` → username; `uri` → a URI claim; `github-workflow`/`gitlab-pipeline`/etc. (via
the `ci-provider` alias block, also fully captured) → a templated
`subject-alternative-name-template` built from multiple claims (e.g. GitLab's
`ci-provider: *gitlab-type` block templates the SAN from `project_path`+`ref`+etc., not shown
in full here but present in the fetched file — available on request). For dex's own
`mockCallback` connector, the returned ID token's `email` claim is whatever the connector's
mock fixture sets — dex's `mock` connector is scriptable to return an arbitrary claim set,
which is what makes `type: email` the right choice for a compose-based test issuer.

**Fulcio's config for the compose setup itself** (`FULCIO_CONFIG` env var, defaults to
`./config/identity/config.yaml` — i.e. **upstream's own compose reuses the production
400-issuer config file wholesale**, relying on the `dex-idp` issuer URL `http://dex-idp:8888/auth`
not being one of the entries — meaning **out of the box this exact compose file would reject
tokens from its own dex-idp**, since `http://dex-idp:8888/auth` isn't in that 400-issuer list).
This is worth flagging precisely because it's the kind of thing a copy-paste of the upstream
compose file would silently break on: **ocx's `FULCIO_CONFIG` must point at a
minimal override file** containing exactly one `oidc-issuers` entry for `http://dex-idp:8888/auth`
with `type: email`, not the full upstream `config/identity/config.yaml`.

**Schema check at the pinned image tag (`ghcr.io/sigstore/fulcio:v1.8.8`, not `main`)** —
fetched `config/identity/config.yaml` and `pkg/config/config.go` at the `v1.8.8` git tag
specifically, since §8 pinned that version and the shape must be checked against what
actually ships in the image, not against `main`. **Unchanged from what's documented
above; nothing has moved to a new form.** `pkg/config/config.go`'s `FulcioConfig` struct
has always had three top-level maps, all optional (`omitempty`): `oidc-issuers` (exact
issuer-URL match — what we need), `meta-issuers` (templated/wildcard issuer URLs like
`https://*.oic.prod-aks.azure.com/*/*`, for cloud-provider workload-identity issuers with
non-fixed URLs — irrelevant for a single fixed dex URL), and `ci-issuer-metadata`
(SAN-template metadata consumed only by `type: ci-provider` entries). `ci-provider` was
never a top-level key or a schema generation — it is, and has always been, one value of
the `type:` field on an `oidc-issuers`/`meta-issuers` entry (visible in both the `main`
and `v1.8.8` fetches, e.g. the GitLab/Buildkite/CircleCI entries). `diff`-ing the two
fetched files shows only issuer-list churn (a handful of CI-provider entries added/removed
between the two refs) — zero schema-shape difference.

`OIDCIssuer`'s full field list (`pkg/config/config.go`, all `omitempty`): `issuer-url`,
`client-id`, `type`, `ci-provider`, `issuer-claim`, `subject-domain`,
`spiffe-trust-domain`, `challenge-claim`, `description`, `contact`, plus an optional
`ca-cert` for a custom-CA'd issuer (not needed here — dex-idp is plain HTTP inside the
compose network). For `type: email`, only `issuer-url`/`client-id`/`type` do anything;
the rest are for other `type` values. `client-id` must be `sigstore` here, not `fulcio` —
this is the same client ID §9.4 requires for `IdentityToken::try_from`'s `aud == "sigstore"`
check, since Fulcio's own issuer verification checks the token's `aud` against this
configured `client-id` too. **Minimal single-issuer `FULCIO_CONFIG` file, verbatim:**

```yaml
oidc-issuers:
  http://dex-idp:8888/auth:
    issuer-url: http://dex-idp:8888/auth
    client-id: sigstore
    type: email
    description: "ocx acceptance-test dex issuer (docker-compose, non-production)"
```

No `meta-issuers`, no `ci-issuer-metadata` block needed — both are optional maps and an
absent map is valid per the struct's `omitempty` tags (confirmed by reading the struct,
not assumed).

Sources: [fulcio config/dex/docker-compose-config.yaml](https://github.com/sigstore/fulcio/blob/main/config/dex/docker-compose-config.yaml), [fulcio config/identity/config.yaml](https://github.com/sigstore/fulcio/blob/main/config/identity/config.yaml), [fulcio config/identity/config.yaml @ v1.8.8](https://github.com/sigstore/fulcio/blob/v1.8.8/config/identity/config.yaml), [fulcio pkg/config/config.go @ v1.8.8](https://github.com/sigstore/fulcio/blob/v1.8.8/pkg/config/config.go) (`FulcioConfig`/`OIDCIssuer` struct definitions), [dex mock connector](https://github.com/dexidp/dex) (connector type name confirmed from the compose config; internal package structure UNCONFIRMED in this pass), [scaffolding testdata/config/gettoken](https://github.com/sigstore/scaffolding/blob/main/testdata/config/gettoken/gettoken.yaml)

---

## 4. Producing the trust root

`cosign trusted-root create` flags, fetched verbatim from `doc/cosign_trusted-root_create.md`:

```
cosign trusted-root create \
    --fulcio="url=https://fulcio.sigstore.dev,certificate-chain=/path/to/fulcio.pem,end-time=2025-01-01T00:00:00Z" \
    --rekor="url=https://rekor.sigstore.dev,public-key=/path/to/rekor.pub,start-time=2024-01-01T00:00:00Z" \
    --ctfe="url=https://ctfe.sigstore.dev,public-key=/path/to/ctfe.pub,start-time=2024-01-01T00:00:00Z" \
    --tsa="url=https://timestamp.sigstore.dev/api/v1/timestamp,certificate-chain=/path/to/tsa.pem" \
    --out trusted-root.json
```

Exact flag semantics (verbatim from the doc):
- `--fulcio`: required keys `url`, `certificate-chain` (PEM path). Optional `start-time`, `end-time`.
- `--rekor`: required keys `url`, `public-key` (PEM path), `start-time`. Optional `end-time`, `origin`.
- `--ctfe`: required keys `url`, `public-key` (PEM path), `start-time`. Optional `end-time`, `origin`.
- `--tsa`: required keys `url`, `certificate-chain` (PEM path). Optional `start-time`, `end-time`.
- `--no-default-{ctfe,fulcio,rekor,tsa}`: strip that service's defaults from the output.
- `--with-default-services`: seed from the public Sigstore TUF root, then let the explicit
  flags above override — **not what a fully self-hosted stack wants**; omit it and specify
  every service explicitly, or the trusted root ends up trusting both the self-hosted stack
  and the public one.

**Extracting the keys from a running compose stack** — runnable sequence, composed from the
compose files above (this exact sequence is not published verbatim upstream; it's
synthesized from the config each service is invoked with):

```bash
# 1. Fulcio's CA root cert — it's the file already mounted into the container,
#    just copy it out (no API call needed, it's the fileca root):
docker compose cp fulcio-server:/etc/fulcio/root.pem ./fulcio-root.pem

# 2. Rekor's public key — classic Rekor v1 with --rekor_server.signer=memory
#    generates its key at startup, so it MUST be scraped, not assumed:
curl -s http://localhost:3000/api/v1/log/publicKey -o rekor.pub

# 2'. Rekor v2 (rekor-tiles posix): the signer key is a checked-in fixture
#     (tests/testdata/pki/ed25519-priv-key.pem) — derive the public key locally,
#     no API round-trip needed, and it's stable across restarts:
openssl pkey -in ed25519-priv-key.pem -pubout -out rekor.pub

# 3. CTFE/tesseract public key — tesseract is configured with a fixed
#    ./config/ctfe/privkey.pem in the compose file, so again derive locally
#    rather than querying the log:
openssl pkey -in config/ctfe/privkey.pem -pubout -out ctfe.pub

# 4. Assemble:
cosign trusted-root create \
  --no-default-fulcio --no-default-rekor --no-default-ctfe --no-default-tsa \
  --fulcio="url=http://localhost:5555,certificate-chain=./fulcio-root.pem,start-time=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --rekor="url=http://localhost:3000,public-key=./rekor.pub,start-time=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --ctfe="url=http://localhost:6962,public-key=./ctfe.pub,start-time=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --out trusted-root.json
```

`sigstore-rs` consumes the same protobuf `trusted_root.json` shape (it's the Sigstore-wide
TUF-distributed format, not cosign-specific) — this file becomes the fixture ocx's acceptance
tests pin, generated once at compose-startup time (or checked in and regenerated only when
the fixture's signer keys rotate, which for the fixed-file cases in §4 step 2'/3 is "never,
unless the fixture file changes").

Sources: [cosign_trusted-root_create.md](https://github.com/sigstore/cosign/blob/main/doc/cosign_trusted-root_create.md), Rekor v1 public-key endpoint path is a well-known Rekor REST API route (`/api/v1/log/publicKey`) — UNCONFIRMED against the exact fetched OpenAPI spec in this pass, cross-check against `rekor/openapi.yaml` before implementing.

---

## 5. Startup cost

No published benchmark exists for this exact compose combination (UNCONFIRMED numbers
throughout this section — nothing here should be quoted as measured until the implementing
agent runs it and records real `hyperfine`/`docker compose up --wait` timings, per this
project's own `PERF-01`). What is directly evidenced from the fetched files:

- **Healthcheck-based readiness is already wired for every relevant service.** Every
  service in both §2a and §2c compose files has an explicit `healthcheck:` block with
  `start_period`/`retries`/`interval` — meaning `docker compose up --wait` (or the existing
  `test/docker-compose.yml` startup pattern) can gate on real readiness rather than a fixed
  sleep. Longest declared `start_period` values: Fulcio 30s, Rekor v1 30s, tesseract 15s,
  rekor-tiles' rekor 30s(via retries×interval)/nginx 30s(×10 retries×3s). Classic Rekor v1's
  MySQL healthcheck alone declares `start_period: 90s` — a strong signal that classic Rekor
  v1 is the slow path and Rekor v2/POSIX (§2c, no MySQL) is materially faster to become ready.
- **RAM**: not stated anywhere in the fetched configs. Qualitatively: classic Rekor v1 pulls
  in MySQL (typically several hundred MB resident) + two Trillian JVM-free-but-Go processes +
  Redis; Rekor v2/POSIX pulls in one Go binary + nginx + a Go witness binary — meaningfully
  lighter. Get a real number by running `docker stats` once the compose is up; do not guess a
  figure into documentation.
- **"Sign once, reuse fixtures" is viable, and is in fact closer to *required* than optional**,
  because of certificate lifetime: **Fulcio-issued code-signing certs are documented as
  ~10-minute validity** (mission brief's own framing, consistent with Sigstore's published
  design — short-lived certs are the entire point of keyless signing, not a self-hosted-stack
  quirk). Consequences for test design:
  - A **signature + Rekor-entry fixture, once produced, is valid forever for *verification***
    (Rekor's inclusion proof and the cert's *validity-at-signing-time*, checked against the
    Rekor-logged timestamp via the SCT/inclusion-proof chain, is what verification checks —
    not "is this cert valid right now"). So a fixture signed once at CI-fixture-generation
    time keeps verifying correctly indefinitely, exactly like real-world cosign-signed
    artifacts still verify years later.
  - What is **not** reusable across a long-lived fixture: anything that needs a **fresh
    signing operation** (testing the sign path itself, or testing against the trust root's
    own `start-time`/`end-time` window from §4 — those bounds must bracket the fixture's
    original signing time, so regenerating the trust root file must not accidentally shrink
    that window).
  - Recommended split: (a) a session-scoped fixture package, signed **once** per
    `pytest_sessionstart` (mirrors this repo's own `registry` fixture pattern in
    `test/conftest.py`) — reused read-only by every verify-path test; (b) a small number of
    tests that must exercise the *live* sign path run their own `ocx sign` invocation against
    the running stack, accepting the ~10-minute cert lifetime as a non-issue since a single
    test run completes in seconds.

Sources: healthcheck data fetched directly from §2a/§2c compose files above; cert-lifetime claim carried from the mission brief (stated there as an established fact, not independently re-verified against a primary Sigstore doc in this research pass — flagged, not contradicted).

---

## 6. Prior art — how sigstore-python/go/js and cosign test against real infra

Confirmed for three of four (sigstore-js not independently checked — UNCONFIRMED, ran out
of budget):

| Project | CI mechanism | Runs its own Fulcio/Rekor? |
|---|---|---|
| `sigstore/cosign` | Doesn't need surveying separately — cosign's *own* dev-compose IS `sigstore/fulcio`+`sigstore/rekor`'s compose files (§2a/§2b); cosign's CI otherwise uses `sigstore-conformance` against public infra like every other client | No, for CI; yes for the repo maintainers' local dev loop |
| `sigstore/sigstore-go` | `.github/workflows/e2e.yml` — `uses: sigstore/scaffolding/actions/setup-sigstore-env@main` (kind-based), needs an explicit multi-tool disk-cleanup step first, then `go test -tags=e2e ./test/e2e` | Yes, but via **kind** (heavy), not compose |
| `sigstore/sigstore-python` | Separate `conformance.yml` **and** `staging-tests.yml` workflows — no `docker-compose` file anywhere in the repo (`gh search code` returned zero hits) | No — hits public staging/production via `sigstore-conformance`'s GHA action |
| `sigstore-conformance` (the cross-client harness) | `action.yml` — runs the target client against `production` (default) or `staging` **public** Sigstore infra, authenticated via GitHub Actions' native OIDC token | No |

**Takeaway for ocx**: no surveyed client repo runs a bare docker-compose Fulcio+Rekor stack
in its own CI — they either (a) hit real public infra via ambient GHA OIDC (lightest, but
means "acceptance tests" secretly depend on the internet and a third party's uptime — a
correctness/isolation regression `subsystem-tests.md` would flag: TEST-07 bans opening a
socket to anything but a local `wiremock`/local fixture in the default profile), or (b) use
kind/scaffolding (heaviest, proven too slow even for GHA-hosted CI without a disk-cleanup
step first). **ocx's own instinct — compose, not kind, following its existing zot/registry:2
pattern — is the right one and is not something any surveyed upstream project does exactly
this way**, which is fine: it's the correct synthesis of "real infra" (unlike wiremock-fake)
+ "fast/local" (unlike kind), assembled from real upstream compose fragments (§2a/§2c) rather
than invented from scratch.

Sources: [sigstore-go e2e.yml](https://github.com/sigstore/sigstore-go/blob/main/.github/workflows/e2e.yml), [sigstore-python workflows listing](https://github.com/sigstore/sigstore-python/tree/main/.github/workflows), [sigstore-conformance action.yml](https://github.com/sigstore/sigstore-conformance/blob/main/action.yml), [scaffolding actions/setup-sigstore-env](https://github.com/sigstore/scaffolding/tree/main/actions/setup)

---

## 7. Documentation angle: self-hosting vs. public-good, and CI snippets

### Self-hosting components (for user-facing docs)

An enterprise self-hosting Sigstore needs everything in §1's "required" column, deployed
durably (not the ephemeral dev-compose from §2): Fulcio + a CT log (TesseraCT, not the
legacy CTFE) + an OIDC issuer *the organization already trusts* (Okta/Dex-fronting-LDAP/
Google Workspace — **not** `mockCallback`, that's test-only) + Rekor (v1 classic or v2/tiles
— Chainguard's bundle still ships classic v1 + Trillian + Redis as of the fetched search
result, so v1 remains the safer default to document for now) + optionally a TSA. **The
trust-root distribution problem**: every verifying client needs the self-hosted stack's
`trusted_root.json`/TUF root, generated exactly as in §4, and — unlike the public Sigstore
instance's TUF-mirrored auto-refreshing root — a self-hosted deployment has no free auto-refresh
that a public deployment automatically gives you via the public TUF root at
`root-signing`; the org either stands up its **own** TUF repo (heavier, out of scope for a
first cut) or documents a manual `trusted_root.json` refresh/pin procedure with an expiry
warning. Say this plainly in docs rather than glossing over it — it is the single biggest
operational cost of self-hosting relative to using the public-good instance.

### External/public-good options

- **Public-good Sigstore**: `fulcio.sigstore.dev` + `rekor.sigstore.dev` (Rekor v1 currently;
  Rekor v2 not yet the public default as of the `rekor-tiles` README, §2c), CT log at
  `ctfe.sigstore.dev` (soon TesseraCT-backed publicly too, per the `ctlog-tiles` Helm chart
  existing) — free, no account needed, `id-token: write` ambient OIDC from GHA/GitLab/others
  is all a client needs. This is what `cosign sign` does with zero config today.
- **Chainguard**: two distinct offerings, don't conflate them in docs — (1) **Chainguard
  Images Sigstore bundle**: 17 low-CVE container images (Fulcio/Rekor/Trillian/CT
  log/Redis/cosign/TSA) for **self-hosting your own instance**, not a hosted service; (2)
  **Chainguard Enforce Signing**: an actual hosted/managed signing product, bring-your-own-key,
  explicitly **does not** publish to a public transparency log (privacy trade-off vs. the
  public-good instance's public-by-design model). [Chainguard Sigstore bundle
  announcement](https://www.chainguard.dev/unchained/chainguard-announces-new-sigstore-images-to-bring-critical-software-supply-chain-tooling-to-enterprises).

### GitHub Actions — keyless signing with ambient OIDC

```yaml
permissions:
  id-token: write   # required: this is what lets the runner mint an OIDC token for Fulcio
  contents: read

jobs:
  sign:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - name: Sign artifact
        run: cosign sign --yes ghcr.io/org/image:tag   # or: ocx <equivalent>
```

No client secret, no static credential: `id-token: write` is what GHA's OIDC token minting
requires; cosign's Fulcio client auto-detects the GHA environment and requests the ambient
token. This is the exact mechanism `sigstore-conformance` (§2e) relies on.

### GitLab CI — keyless signing with ambient OIDC

```yaml
sign:
  stage: sign
  id_tokens:
    SIGSTORE_ID_TOKEN:
      aud: sigstore   # audience Fulcio expects; must match Fulcio's configured OIDC audience
  script:
    - cosign sign --yes --identity-token="$SIGSTORE_ID_TOKEN" ghcr.io/org/image:tag
```

GitLab's ambient-OIDC mechanism is `id_tokens:` (not a permissions block like GHA) — it mints
a JWT into the named env var (`SIGSTORE_ID_TOKEN` here, but any name works) scoped to the
declared `aud:`. Unlike GHA, the token isn't picked up automatically by cosign's environment
auto-detection — it's passed explicitly via `--identity-token`.

**Both snippets are hand-composed from documented, standard mechanisms** (`id-token: write`
is a well-established GHA permissions key; `id_tokens:`/`aud:` is a well-established GitLab
CI keyword) — neither was copy-pasted from a fetched sigstore doc page in this pass (time
budget). Flag for a follow-up fetch of `docs.sigstore.dev`'s own CI-integration pages before
this goes into user-facing docs verbatim, to confirm cosign's exact env-var auto-detection
name for GHA and the precise recommended `aud:` value for GitLab (`sigstore` is the
conventional default seen across the ecosystem, e.g. dex's own `staticClients` audience
naming pattern in §3, but not independently re-confirmed against a GitLab-specific Sigstore doc).

---

## 8. Follow-up: image sourcing and the Rekor version decision

> **Editorial note.** A second, uncredited pass appended a conflicting answer to this same
> section while this one was in progress (visible in git history/session logs, not reproduced
> here). Both tables were independently real — I re-verified every disputed image reference
> live via `docker manifest inspect` before reconciling — but they made two different
> architectural calls. This is the single, reconciled answer; the conflict and its resolution
> are recorded at the end of §8b so the disagreement isn't silently erased.

### Blocker B verdict first, because it decides what Blocker A even needs to source

**Classic Rekor v1 is the only option — `rekor-tiles`/v2 is a dead end for this client, and
there is no lighter way to run v1.** Three confirmed facts, in order:

1. **`sigstore-rs 0.14` has zero Rekor v2 client surface** (per
   `.claude/artifacts/research_sigstore_rs_spike.md`, which I re-read for this follow-up: only
   `rekor::apis::entries_api::create_log_entry` against the v1 `hashedrekord:0.0.1` proposal
   type exists). So §2c's `posix-compose.yml` recommendation is withdrawn — it would stand up
   a server our own Rust client cannot talk to.
2. **`rekor-tiles` exposes no v1-compatible REST surface to fall back to.** Its `api/`
   directory (fetched) contains only `proto` — gRPC + protobuf, no OpenAPI/REST layer at all,
   let alone a `/api/v1/log/entries`-compatible one. A clean "no" to blocker B question 3.
3. **There is no lighter classic-Rekor-v1 storage than Trillian+MySQL in the shipped
   binary**, confirmed by reading the actual Go source, not inferring from a compose file:
   - `google/trillian`'s `storage/memory` package **exists** and self-registers a `"memory"`
     provider (`storage.RegisterProvider("memory", ...)`, confirmed by reading
     `storage/memory/provider.go`) — this is what made me check further rather than take the
     compose file's MySQL dependency at face value.
   - But `cmd/trillian_log_server/main.go` imports its storage providers through exactly one
     package, `github.com/google/trillian/cmd/internal/provider`, and that package's directory
     listing (fetched) is `cloudspanner.go crdb.go default_systems.go etcd.go k8s.go mysql.go
     postgresql.go` — **no `memory.go`**. The memory provider is never blank-imported into the
     binary that ships in `ghcr.io/sigstore/rekor/trillian_log_server`, so
     `storage.Providers()` inside that binary never contains `"memory"` and
     `--storage_system=memory` is not a reachable flag value — it 404s at the Go
     `flag`/provider-lookup layer, not at some documented-but-untested edge. Confirmed doubly
     by reading `cmd/internal/provider/default_systems.go`: `defaultProvider := "mysql"`,
     falling back only to CockroachDB/Postgres/Spanner if MySQL isn't registered — never to
     `memory`.
   - **Answer to blocker B question 1: no.** The only selectable backends in the real,
     published `trillian_log_server`/`trillian_log_signer` images are MySQL, PostgreSQL,
     CockroachDB, and Cloud Spanner. All four need a real database service. There is no
     single-binary/no-DB dev mode for classic Rekor v1.
   - **Answer to question 4** (a purpose-built lightweight Rekor v1 emulator, real enough for
     a canonical SET): not found upstream in this pass. Not inventing one, per the brief's own
     non-negotiable against hand-rolling — the correct reading of "no lighter option exists"
     is "run the real MySQL+Trillian stack", not "build a substitute".

**One further simplification, found while re-reading §2b's own quoted compose against itself:**
upstream's `rekor/docker-compose.yml` declares a `redis-server` service, but `rekor-server`'s
`command:` array (quoted verbatim in §2b) passes `--search_index.storage_provider=mysql`, never
`redis` — nothing in the quoted command references `redis-server` at all. Redis backs only
Rekor's search-by-email/hash index, ocx never searches the log (it uploads one entry and
verifies one SET), and upstream's own compose has already routed the search index through MySQL
instead. **`redis-server` is droppable** from the ocx compose file — one fewer service, one
fewer image, no functional loss for what ocx's sign/verify pipeline does.

**Consequence for `test/docker-compose.yml` startup-cost placement (answers question 2):**
`db_server`'s own healthcheck in the *upstream* rekor compose (§2b, already quoted) declares
`start_period: 90s`, and `rekor-server`'s healthcheck on top of that declares another
`start_period: 30s` chained after `trillian-log-server`'s `service_healthy` — these are
upstream Sigstore's own numbers for the exact images being reused here, not a guess. That is
too slow for `task test`'s default path to eat on every invocation (contrast: this repo's
existing `registry`/`mirror-registry` services have no comparable startup tax). **Recommendation:
put the full Rekor v1 stack (mysql + trillian-log-server + trillian-log-signer + rekor-server)
behind an opt-in Docker Compose profile, exactly like the existing `bench-proxy` profile
(`test/docker-compose.yml:77-84`) — e.g. a `sigstore` profile — rather than the default
services list.** Signing/verify acceptance tests that need it opt in the same way
`task bench:setup` opts into `--profile bench`; the fast default `task test` path stays fast.
This is a design decision for the ADR to ratify, not something already decided here — flagging
it as the concrete number (90s+30s = at least 2 minutes of healthcheck budget, before Fulcio,
dex, and tesseract's own start periods are even added) that should drive that ADR call.

### Blocker A: prebuilt images, one row per service, with an anonymous-pull existence proof

Method: `docker manifest inspect <ref>` (Docker 29.5.2, confirmed working, network-unrestricted
in this environment) against the exact tag GitHub's package API reports as that package's
newest real release tag (`gh api orgs/sigstore/packages/container/<name>/versions`) — not
`:latest`, which 404s on more than one of these. Every row below is a command that was run and
returned a real manifest, not an inference from a Helm `values.yaml`.

| Service | Published image | Tag used to verify | Architectures | Command-line flags (from the upstream `command:` arrays already captured in §2) | Config to mount |
|---|---|---|---|---|---|
| **Fulcio server** | `ghcr.io/sigstore/fulcio` | `v1.8.8` | amd64, arm, arm64, ppc64le, s390x (OCI index confirmed) | `fulcio-server serve --host=0.0.0.0 --port=5555 --grpc-port=5554 --ca=fileca --fileca-cert=/etc/fulcio/root.pem --fileca-key=/etc/fulcio/root.key --fileca-key-passwd=fulcio --ct-log-url=http://tesseract:6962` (verbatim from §2a) | `/etc/fulcio/root.pem`+`/etc/fulcio/root.key` — **a self-signed test CA we generate ourselves** (`openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P256 ...`); no published "test CA" artifact exists, this is expected to be hand-rolled infra, not signing logic. `/etc/fulcio-config/config.yaml` — **the single-issuer override**, not upstream's 400-issuer file (see §3's landmine) |
| **CT log** | `ghcr.io/transparency-dev/tesseract/posix` (upstream-canonical) **or** `ghcr.io/sigstore/scaffolding/tesseract/posix` (sigstore's own build of the same thing) | `v0.1.1` (upstream) / `v0.7.33` (sigstore) | amd64, arm64 confirmed on `rekor-tiles/posix:main`'s index as a proxy for the tesseract-family build pipeline; tesseract itself not independently manifest-checked (UNCONFIRMED — do before implementing) | `--private_key /etc/ctfe/privkey.pem --origin tesseract --storage_dir /ctfe --roots_pem_file /etc/fulcio/root.pem --v 1 --ext_key_usages CodeSigning --http_endpoint 0.0.0.0:6962` (verbatim from §2a) | `/etc/ctfe/privkey.pem` (self-generated), `/etc/fulcio/root.pem` (same CA as Fulcio) |
| **CT log, classic (fallback only)** | **No runnable image found.** `ghcr.io/sigstore/helm-charts/ctlog:0.2.67` resolves but its manifest is `application/vnd.cncf.helm.config.v1+json` + `.tar+gzip` chart layers — **a Helm chart artifact, not a container image** (confirmed by reading the raw manifest). If classic CTFE is ever wanted instead of TesseraCT, the actual image ref is inside that chart's `values.yaml`, not directly pullable as-is | — | — |
| **OIDC issuer** | `dexidp/dex` | `v2.45.1` | confirmed multi-arch: amd64, arm, arm64, ppc64le, s390x | `dex serve /etc/config/docker-compose-config.yaml` | Fulcio's wrapper `Dockerfile.dex-idp` only adds a `COPY ./config/dex /etc/config/` on top of this exact image — **skip the wrapper build entirely and bind-mount the config directory instead**: `volumes: [./dex-config:/etc/config:ro]`, using the `docker-compose-config.yaml` content already quoted verbatim in §3 |
| **Rekor v1 server** | `ghcr.io/sigstore/rekor/rekor-server` | `v1.5.3` | amd64, arm, arm64, ppc64le, s390x (OCI index confirmed) | `rekor-server serve --trillian_log_server.address=trillian-log-server --trillian_log_server.port=8090 --rekor_server.address=0.0.0.0 --rekor_server.signer=memory --enable_attestation_storage --attestation_storage_bucket=file:///var/run/attestations --search_index.storage_provider=mysql --search_index.mysql.dsn=...` (verbatim from §2b) | none beyond the attestation-storage volume; signer key is ephemeral (`signer=memory`) — **must be scraped post-startup** via `/api/v1/log/publicKey` (route confirmed against `rekor/openapi.yaml`, closing an earlier UNCONFIRMED flag), not assumed stable across restarts |
| **Trillian log server** | `ghcr.io/sigstore/rekor/trillian_log_server` | `v1.3.4` | amd64, 386, arm, arm64, ppc64le, s390x (OCI index confirmed — the widest arch list of anything checked) | `--quota_system=noop --storage_system=mysql --mysql_uri=... --rpc_endpoint=0.0.0.0:8090 --http_endpoint=0.0.0.0:8091 --alsologtostderr` (verbatim from §2b) | none — schema comes from the `db_server` image below |
| **Trillian log signer** | `ghcr.io/sigstore/rekor/trillian_log_signer` | `v1.3.4` (same package family, verified to exist via the GH packages API; manifest not independently re-checked but is the same build pipeline as `trillian_log_server` — low risk) | same as log server (UNCONFIRMED independently, high-confidence by construction) | `--quota_system=noop --storage_system=mysql --mysql_uri=... --rpc_endpoint=0.0.0.0:8090 --http_endpoint=0.0.0.0:8091 --force_master --alsologtostderr` (verbatim from §2b) | none |
| **Trillian DB** | `gcr.io/trillian-opensource-ci/db_server` | `v1.4.0` | confirmed pullable (Docker v2 schema-2 single-platform manifest, i.e. **not a multi-arch OCI index** — treat as amd64-only until proven otherwise; fine for this repo's `ubuntu-latest`/WSL2-amd64 CI and dev environment per the mission brief's environment facts, but flag as an arm64 gap for e.g. Apple Silicon contributors) | `MYSQL_ROOT_PASSWORD=zaphod MYSQL_DATABASE=test MYSQL_USER=test MYSQL_PASSWORD=zaphod` (verbatim from §2b) | none — this image already bundles Trillian's schema init, unlike a vanilla `mysql:8` which would need `docker-entrypoint-initdb.d/schema.sql` added by hand |
| **Rekor v2 (rekor-tiles, posix)** | `ghcr.io/sigstore/rekor-tiles/posix` | `main` | amd64, arm64 (OCI index confirmed) | n/a — **not usable, see Blocker B verdict above; kept here only to record that the image genuinely exists and is real, in case sigstore-rs gains v2 support later** | n/a |

**Net answer to Blocker A**: every component needed for the Blocker-B-mandated classic-Rekor-v1
stack has a real, pullable, `build:`-free image — Fulcio, dex, Trillian×2, the Trillian MySQL
image, and Rekor v1 server are all confirmed. The one gap is the CT log: TesseraCT
(`ghcr.io/transparency-dev/tesseract/posix` or `ghcr.io/sigstore/scaffolding/tesseract/posix`)
is real and pullable but its own multi-arch status wasn't independently manifest-checked in
this pass (do it before implementing); classic CTFE has **no runnable published image at all**
under `sigstore/helm-charts/ctlog` — that package is a Helm chart artifact, not a container —
so TesseraCT is not just the modern choice (§1's correction) but the *only* CT log with a
confirmed pullable image. No `build:` stanza is required anywhere in the resulting compose file.

**Resolving the conflict with the other pass's answer.** The competing table (superseded by
this one) proposed an older, self-consistent pairing instead: `gcr.io/projectsigstore/fulcio:v1.7.1`
+ `ghcr.io/sigstore/scaffolding/ct_server:latest` (classic RFC 6962 CTFE), on the reasoning that
pairing *current* Fulcio with TesseraCT is "an untested cross-generation mix." I checked that
claim directly rather than taking either side on faith, and it is **half right**: TesseraCT's
own README states plainly, "**API: TesseraCT implements static-ct-api rather than RFC6962**"
(`transparency-dev/tesseract/README.md`) — it is genuinely a different submission wire protocol
from classic CTFE, not RFC 6962 served by different storage. So a real version boundary exists
somewhere in Fulcio's CT-log client, and mixing an RFC-6962-era Fulcio with a static-ct-api log
(or vice versa) would break. But the conclusion the other pass drew from that fact is backwards:
**the pairing with direct evidence of actually working is current Fulcio + TesseraCT, not v1.7.1
+ ct_server** — because `sigstore/fulcio`'s own live `main`-branch `docker-compose.yml` (fetched
verbatim in §2a, and the newest published tag, `v1.8.8`, tracks that same branch) is exactly that
pairing, maintained and (presumably) exercised by the Fulcio team's own dev loop. Reaching back
to `v1.7.1` to get an RFC-6962-speaking Fulcio is a real, valid, alternative path — but it is the
one requiring an old release and an untested-by-*this*-research claim that v1.7.1's CT-log client
is unmodified back to RFC 6962, not the current-main pairing this file already sourced directly
from upstream. **Verdict: keep `ghcr.io/sigstore/fulcio:v1.8.8` + TesseraCT** (this table);
`gcr.io/projectsigstore/fulcio:v1.7.1` + `ct_server` is recorded as a real, working, but
not-recommended fallback if TesseraCT's own image turns out to have a real blocking problem
(still open per item 6 below). Two negative claims from the superseded table are corrected here
because they were tag-path errors, not registry-wide absences, and are worth recording so nobody
re-derives them: `ghcr.io/sigstore/fulcio:v1.7.4` genuinely doesn't exist, but `ghcr.io/sigstore/fulcio:v1.8.8`
does — Fulcio publishes to **both** `ghcr.io/sigstore` and `gcr.io/projectsigstore`, contrary to
the superseded table's "Fulcio publishes to gcr.io/projectsigstore, not ghcr.io/sigstore" line
(directly disproved by `docker manifest inspect ghcr.io/sigstore/fulcio:v1.8.8` exiting 0);
similarly `ghcr.io/sigstore/rekor-server` (flat) doesn't exist, but the correctly-nested
`ghcr.io/sigstore/rekor/rekor-server:v1.5.3` does (also `docker manifest inspect`-confirmed) —
the superseded table found the flat path's absence and stopped, rather than checking the GitHub
Packages API for the real (nested) package name the way this pass did.

Sources for this section: `docker manifest inspect` runs against `ghcr.io/sigstore/fulcio:v1.8.8`, `ghcr.io/sigstore/rekor/rekor-server:v1.5.3`, `ghcr.io/sigstore/rekor-tiles/posix:main`, `ghcr.io/sigstore/helm-charts/ctlog:0.2.67`, `ghcr.io/sigstore/rekor/trillian_log_server:v1.3.4`, `dexidp/dex:v2.45.1`, `gcr.io/trillian-opensource-ci/db_server:v1.4.0`; `gh api orgs/sigstore/packages/container/*/versions` for real tag discovery; [google/trillian storage/memory/provider.go](https://github.com/google/trillian/blob/master/storage/memory/provider.go), [cmd/internal/provider directory listing](https://github.com/google/trillian/tree/master/cmd/internal/provider), [cmd/internal/provider/default_systems.go](https://github.com/google/trillian/blob/master/cmd/internal/provider/default_systems.go), [cmd/trillian_log_server/main.go](https://github.com/google/trillian/blob/master/cmd/trillian_log_server/main.go), [sigstore/rekor-tiles api/ directory](https://github.com/sigstore/rekor-tiles/tree/main/api) (proto-only, confirmed no REST layer), [sigstore/rekor openapi.yaml `/api/v1/log/publicKey`](https://github.com/sigstore/rekor/blob/main/openapi.yaml).

---

## 9. Non-interactive OIDC token acquisition

**Verdict up front: option 3, dex's built-in `local`/`staticPasswords` connector via the
Resource Owner Password Credentials grant, wins.** It is dex's real, documented,
production connector (not an internal test fixture), the wire exchange is a single
`POST /token`, and — unlike every mock connector — it lets the test harness pin the
exact `email` claim that becomes the Fulcio certificate SAN. Option 1 is dead
(Kubernetes-only). Option 2 (device-code) was investigated far enough to show it adds a
polling round trip for zero benefit once the connector itself needs no human step, and a
plain OAuth2 authorization-code exchange against `mockCallback` is the simpler cousin of
option 2 — presented below as the minimal-config fallback. Option 4 (sigstore-rs itself)
is not a token-acquisition mechanism at all — it is the consumption seam: the crate
accepts a pre-minted JWT string directly, with one hard-coded constraint that changes how
every dex client must be configured.

### 9.1 Option 1 — `getoidctoken`: rejected, Kubernetes-only

`sigstore/scaffolding/tools/getoidctoken/cmd/getoidctoken/main.go` (fetched in full) is a
14-line HTTP server: `http.HandleFunc("/", tokenWriter(env.FileName))` on `:8080`, which
just reads and serves the raw contents of a file (`OIDC_FILE` env var, default
`/var/run/sigstore/cosign/oidc-token`). It performs **no OIDC/OAuth logic of its own** —
the token in that file is minted entirely by Kubernetes itself via a projected
`serviceAccountToken` volume (`scaffolding/testdata/config/gettoken/gettoken.yaml`,
`audience: sigstore`), which is a k8s-native mechanism with no docker-compose analogue.
Per the standing instruction ("if k8s-only, say so and move on"): rejected, no further
investigation.

### 9.2 Option 2 — device-code flow, and the simpler authorization-code path it leads to

`mockCallback`'s Go source (`sigstore/fulcio`'s vendored dex, but the connector itself
lives upstream at `dexidp/dex/connector/mock/connectortest.go`, fetched in full) shows
`Callback.LoginURL` returns a redirect URL immediately — no form, no consent screen, no
credential check — and `HandleCallback` returns a **hardcoded** identity unconditionally.
This is true for *both* the authorization-code and device-code flows, since both route
through the same connector interface. Device-code (RFC 8628) exists to let a **second,
less-capable device** display a code for a human to type into a browser elsewhere — a
problem `mockCallback` already doesn't have, because there is no human step at the
connector layer to relocate. Chasing the device-code `/device/code` → poll `/device/token`
sequence was not pursued past this point: it is strictly more round trips than plain
authorization-code for the same non-interactive outcome, so it does not "win" over what it
was meant to replace.

**What plain OAuth2 authorization-code against `mockCallback` looks like**, derived from
dex's source rather than an empirical run against a live instance (flag accordingly — the
implementing agent should confirm the exact hop count on first real `docker compose up`):

Fulcio's own dex config sets `alwaysShowLoginScreen: true` — a field whose doc comment
(`cmd/dex/config.go`) is literally "show the connector selection screen **even if
there's only one**" (default `false`). That is, upstream's own compose config
deliberately keeps a human-facing chooser page even though `mockCallback` is the only
connector — a demo/debugging courtesy, not a requirement. **ocx's own dex config should
omit it** (or set it `false` explicitly), which makes dex auto-select the single
connector and collapses the flow to two HTTP round trips:

```sh
# 1. Kick off the auth-code flow. No PKCE required (fulcio's dex config sets no
#    oauth2.pkce.enforce, and it defaults false). Chase Location headers manually
#    instead of -L, because the last hop points at a redirect_uri nothing is listening
#    on — we only need the `code` query param out of it, never to actually fetch it.
curl -s -D - -o /dev/null \
  "http://dex:8888/auth?response_type=code&client_id=sigstore&redirect_uri=http://localhost:0/callback&scope=openid+email&state=xyz"
# -> follow the chain of `Location:` headers (mockCallback's LoginURL redirects back into
#    dex's own /callback handler, which then redirects to redirect_uri?code=...&state=xyz)
#    until the Location host is our own redirect_uri; extract `code` from its query string.

# 2. Exchange the code for tokens.
curl -s -X POST "http://dex:8888/token" \
  --data-urlencode "grant_type=authorization_code" \
  --data-urlencode "code=<code from step 1>" \
  --data-urlencode "redirect_uri=http://localhost:0/callback" \
  --data-urlencode "client_id=sigstore"
# -> { "id_token": "<JWT>", "access_token": "...", ... }
```

This works with **zero config changes** beyond what fulcio's own dex config already has
(swap `alwaysShowLoginScreen` off), and needs no client secret because `staticClients`
entries are `public: true`. Its only weakness is the one in the verdict: the `email`
claim on the resulting token is dex's hardcoded `kilgore@kilgore.trout`, not something
the test author chose.

### 9.3 Option 3 — the password connector, and why it beats both mocks

Two distinct dex mechanisms answer to "password connector," and they behave very
differently:

**(a) `mockPassword`** — same `connector/mock/connectortest.go` file, registered as type
string `"mockPassword"` (confirmed against `server/connector.go`'s registration table:
`"mockPassword": func() connectors.ConnectorConfig { return new(mock.PasswordConfig) }`).
Config is `{username, password}`; `Login()` checks the submitted credentials against
those two configured values and, on match, returns the **same hardcoded Kilgore Trout
identity** as `mockCallback` — the credentials gate access, they do not shape the
returned claims. No better than option 2 for claim control.

**(b) dex's real local password database** — this is the "`staticPasswords` + `type:
local`" shape asked about, though the mechanism is slightly different from a `type:
local` line inside `connectors:`. It is a **top-level server feature**:
`enablePasswordDB: true` plus a top-level `staticPasswords:` list, auto-registered by dex
internally under the fixed connector ID `"local"` (`server/connectors/resolve.go`:
`const LocalConnector = "local"`; `Resolver()` special-cases `conn.Type ==
LocalConnector` to return a `passwordDB` backed by bcrypt comparison, bypassing the
`connectors:` config map entirely). Real config block (`email`/`hash`/`username`/`userID`
fields confirmed against dex's own `config.dev.yaml`, `hash` is the well-known bcrypt
digest of the plaintext `password` dex ships in its own example configs):

```yaml
# On top of the mockCallback connectors: block already present for §3's flow —
# this is additive, not a replacement.
enablePasswordDB: true
staticPasswords:
  - email: "ocx-acceptance-test@example.invalid"   # <- this IS the cert SAN, chosen by us
    hash: "$2a$10$2b2cU8CPhOTaGrs1HRQuAueS7JTT5ZHsHSzYiFPm1leZck7Mc8T4W"  # bcrypt("password")
    username: "ocx-acceptance-test"
    emailVerified: true
    userID: "00000000-0000-0000-0000-000000000001"

oauth2:
  passwordConnector: local   # routes grant_type=password to the connector above
```

`local` needs an OAuth2-confidential client — `staticClients` needs a `secret:` field for
whichever client ID drives this grant (a `public: true` client cannot use HTTP Basic
client auth). One HTTP POST, no redirect chasing at all:

```sh
curl -s -X POST "http://dex:8888/token" \
  -u "sigstore:<client-secret-from-staticClients>" \
  --data-urlencode "grant_type=password" \
  --data-urlencode "scope=openid email" \
  --data-urlencode "username=ocx-acceptance-test" \
  --data-urlencode "password=password"
# -> { "id_token": "<JWT with email=ocx-acceptance-test@example.invalid>", ... }
```

(Exact request shape confirmed against dex's own `server_grant_password_test.go`:
form-urlencoded `grant_type=password&username=...&password=...` plus HTTP Basic client
auth — this is the one part of §9 independently confirmed against a **test file**, not
inferred from handler code alone.)

This is why option 3 (in its real, non-mock form) is the recommended path: it is a
single round trip, it is dex's stable public config surface rather than an internal test
fixture (`connectortest.go`'s own package doc says it exists "to test various server
components" — nothing warns it is a supported integration point, and its returned
identity could change across a dex bump with no changelog entry), and it makes the
certificate SAN a value ocx's own test fixtures choose and assert against, rather than a
string borrowed from a Kilgore Vonnegut novel.

### 9.4 Option 4 — sigstore-rs's own surface: not acquisition, but the consumption seam

`sigstore-rs` 0.14.0's `oauth` module (`~/.cargo/registry/.../sigstore-0.14.0/src/oauth/`,
both `openidflow.rs` and `token.rs` read in full) splits cleanly into two halves that
answer two different questions:

- **`oauth::openidflow` is interactive-only.** `OpenIDAuthorize::auth_url[_async]` does
  OIDC discovery and builds a browser authorize URL; `RedirectListener::redirect_listener`
  then **binds a real `TcpListener`** on a caller-supplied host:port and blocks waiting
  for an actual HTTP redirect to land on it before it can extract the code. There is no
  parameter to skip this and inject a code or token directly — a test harness cannot call
  through this module without standing up a real local HTTP listener and driving a real
  redirect at it, which is possible but strictly more moving parts than curl-ing dex
  directly per §9.2/§9.3.
- **`oauth::token::IdentityToken` is the actual answer to "does it accept a pre-minted
  token string": yes.** `impl TryFrom<&str> for IdentityToken` parses a raw JWT
  (base64url-decodes the middle segment, deserializes it as `Claims { aud, exp, nbf?,
  email }`) with **exactly one hard-coded gate**:

  ```rust
  if claims.aud != "sigstore" {
      return Err(SigstoreError::IdentityTokenError("Not a Sigstore JWT".into()));
  }
  ```

  This is the load-bearing detail for every config block above: **the OIDC client ID used
  to mint the token must be the literal string `sigstore`**, because a standard OIDC
  provider (dex included) sets the ID token's `aud` claim to the requesting client's
  `client_id`. Fulcio's own upstream dex config uses `staticClients: [{id: fulcio, ...}]`
  — copied verbatim, every token minted against it would carry `aud: "fulcio"` and
  `IdentityToken::try_from` would reject it outright with "Not a Sigstore JWT" before ocx
  ever reaches Fulcio. **ocx's own dex config needs a `staticClients` entry with `id:
  sigstore`** (the curl examples in §9.2/§9.3 above already use `client_id=sigstore` for
  this reason), independent of whatever client ID Fulcio itself expects on the CSR path.
  `Claims` also requires `exp` and `email` to be present and typed correctly — `nbf` is
  optional — so a token missing `email` (e.g. one that didn't request the `email` scope)
  fails to parse here even before Fulcio would reject it.

  There is no size/algorithm/signature check inside `TryFrom` itself — it is a claims
  *shape* parser, not a verifier; signature verification happens downstream, wherever
  Fulcio's own cert-issuance endpoint validates the token against the issuer's JWKS. This
  file did not trace which lower-level Fulcio-client call in `sigstore-rs` actually
  consumes an `IdentityToken` (ocx currently bypasses `SigningSession` entirely per the
  mission brief's non-negotiables list) — that remains a question for whoever wires the
  acceptance-test harness's token into ocx's existing `FulcioClient::request_cert_v2` call,
  not one this pass answers.

### 9.5 SAN-claim mapping, stated plainly

Under the minimal `FULCIO_CONFIG` override already established in §3 (`type: email` for
the one `oidc-issuers` entry pointing at `http://dex-idp:8888/auth`), Fulcio embeds the
verified `email` claim as the certificate's SAN, provided `email_verified: true`
accompanies it (`emailVerified: true` in the `staticPasswords` entry above supplies
exactly that). Whatever `email` claim the token carries when it reaches Fulcio is what
`ocx --certificate-identity <value>` must match at verify time — no other claim path
applies under `type: email`.

- **`mockCallback` / `mockPassword`**: the claim value is **not configurable** — it is a
  literal hardcoded in dex's Go source (`kilgore@kilgore.trout`), the same for every token
  either mock connector ever issues, and not documented as stable across dex versions.
- **`local`/`staticPasswords`**: the claim value **is** fully configurable — whatever
  `email:` string a `staticPasswords` entry declares, verbatim.

### 9.6 Already-closed items — not redone here

The task also asked, budget permitting, to close two flags. Both were **already closed in
§8** of this same file, before this section existed:

- **Rekor v1 public-key REST route** — closed: `GET /api/v1/log/publicKey`, confirmed
  directly against `sigstore/rekor/openapi.yaml` (see Open Items list, item 2).
- **`docs.sigstore.dev` canonical CI snippets** — partially closed: the GitHub Actions
  snippet was fetched directly from `docs.sigstore.dev/quickstart/quickstart-ci/`
  (`sigstore/cosign-installer@v4.0.0` under `permissions: id-token: write`). A GitLab CI
  snippet was searched for on that page and not found; §7's GitLab example remains
  hand-composed from GitLab's documented `id_tokens:`/`aud:` mechanism rather than copied
  from an official Sigstore doc page — this one genuinely stays open (item 5 in the Open
  Items list already reflects this precisely).

Sources for this section: [dexidp/dex connector/mock/connectortest.go](https://github.com/dexidp/dex/blob/master/connector/mock/connectortest.go), [dexidp/dex server/connector.go](https://github.com/dexidp/dex/blob/master/server/connector.go) (registration table), [dexidp/dex server/connectors/resolve.go](https://github.com/dexidp/dex/blob/master/server/connectors/resolve.go) (`LocalConnector = "local"`), [dexidp/dex server/connectors/password.go](https://github.com/dexidp/dex/blob/master/server/connectors/password.go) (bcrypt-backed `passwordDB`), [dexidp/dex cmd/dex/config.go](https://github.com/dexidp/dex/blob/master/cmd/dex/config.go) (`AlwaysShowLoginScreen`, `PasswordConnector`, `EnablePasswordDB`, `StaticPasswords` field definitions), [dexidp/dex config.dev.yaml](https://github.com/dexidp/dex/blob/master/config.dev.yaml) (real `staticPasswords` example block), [dexidp/dex server/server_grant_password_test.go](https://github.com/dexidp/dex/blob/master/server/server_grant_password_test.go) (exact ROPC wire request), [dexidp/dex server/config.go](https://github.com/dexidp/dex/blob/master/server/config.go) (`PasswordConnector` server-side wiring), [sigstore/scaffolding tools/getoidctoken/cmd/getoidctoken/main.go](https://github.com/sigstore/scaffolding/blob/main/tools/getoidctoken/cmd/getoidctoken/main.go), vendored `sigstore-0.14.0/src/oauth/openidflow.rs` and `token.rs` (`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sigstore-0.14.0/src/oauth/`, read in full).

---

## Open items / UNCONFIRMED flags for the implementing agent

1. ~~`sigstore-rs`'s exact non-interactive OIDC token acquisition path~~ — **closed in §9**:
   `oauth::openidflow` is interactive/browser-only, but `IdentityToken::TryFrom<&str>`
   accepts a pre-minted JWT directly (hard requirement: `aud == "sigstore"`). Recommended
   acquisition mechanism is dex's `local`/`staticPasswords` connector (§9.3), not
   `mockCallback` — the latter's claims are hardcoded and not configurable. **Still open**:
   the §9.2/§9.3 curl sequences are derived from dex's source and a `server_grant_password_test.go`
   fixture, not run against a live dex instance in this pass — confirm hop count and exact
   response shape on first real `docker compose up`.
2. ~~Rekor v1 public-key REST route~~ — **closed in §8**: confirmed `GET /api/v1/log/publicKey`
   directly against `sigstore/rekor/openapi.yaml`.
3. ~~Fulcio/rekor-server/trillian/dex multi-arch status~~ — **closed in §8** for Fulcio
   (`v1.8.8`), Rekor server (`v1.5.3`), Trillian log server (`v1.3.4`), and dex (`v2.45.1`), all
   confirmed multi-arch via `docker manifest inspect`. Still open: TesseraCT's own image
   (deployed via a proxy inference, not independently checked), Trillian log signer
   (assumed same as log server by build-pipeline construction, not independently checked), and
   `gcr.io/trillian-opensource-ci/db_server` (confirmed single-platform only, not multi-arch).
4. RAM numbers for §5 are still explicitly not measured — get real ones from a live
   `docker compose up --wait` + `docker stats` run before writing them into any doc or ADR
   budget line, per this project's own `PERF-01`. The wall-clock side is now partially
   evidenced: §8 surfaces upstream's own `start_period: 90s` (MySQL) + `30s` (Rekor server)
   healthcheck numbers, which is real (if conservative/upper-bound) upstream data, not a guess.
5. `docs.sigstore.dev`'s canonical CI snippets — **partially closed**: fetched
   `docs.sigstore.dev/quickstart/quickstart-ci/` directly and confirmed the real GitHub Actions
   snippet uses `sigstore/cosign-installer@v4.0.0` under `permissions: id-token: write`, exactly
   as this file's §7 already stated the mechanism (the page itself installs cosign but doesn't
   show a full `cosign sign` invocation inline). **Still open**: no GitLab CI snippet was found
   on that page or elsewhere in this pass — §7's GitLab snippet remains hand-composed from the
   documented `id_tokens:`/`aud:` GitLab CI mechanism, not copied from a Sigstore doc page;
   confirm against a GitLab-specific Sigstore integration doc before shipping verbatim.
6. **New**: TesseraCT image multi-arch status (both `ghcr.io/transparency-dev/tesseract/posix`
   and `ghcr.io/sigstore/scaffolding/tesseract/posix`) — not manifest-checked in this pass,
   should be closed before the compose file is written since it's the one CT log component
   with a confirmed-real but arch-unverified image.
7. **New**: the classic CTFE image reference buried inside the
   `ghcr.io/sigstore/helm-charts/ctlog:0.2.67` chart's `values.yaml` was not extracted — only
   relevant if TesseraCT is ever rejected in favor of classic CTFE, which §1/§8 both argue
   against.
8. **New**: whether `gcr.io/projectsigstore/fulcio:v1.7.1`'s CT-log client genuinely still
   speaks unmodified RFC 6962 (the fallback path recorded in §8's reconciliation note) was
   asserted by the superseded pass, not independently verified by this one — only relevant if
   item 6 (TesseraCT's own image) turns up a real blocker.

### 8c. Orchestrator note — the "second pass" was me, and the worker won the disputed points

The conflicting Blocker-A answer reconciled in §8 above was written by the orchestrator, not
an anonymous third party. Recording the outcome plainly, because the reasoning matters more
than the attribution:

- **`ghcr.io/sigstore/fulcio` does publish Fulcio.** My claim that it does not was wrong: I
  probed `:v1.7.4`, a tag that does not exist, and concluded the registry path was wrong
  rather than the tag. `ghcr.io/sigstore/fulcio:v1.8.8` resolves and is multi-arch
  (amd64, arm, arm64, ppc64le, s390x). `ghcr.io/sigstore/rekor/rekor-server:v1.4.2` likewise
  resolves under the nested path. The worker's live re-verification was the correct move and
  its image set stands as canonical.
- **`ghcr.io/sigstore/fulcio:latest` does NOT exist** — pin an explicit version tag.
- **The missing piece in both passes was the CT log image.** `ghcr.io/transparency-dev/tesseract:latest`
  does not exist; the published images are **`ghcr.io/transparency-dev/tesseract/posix:latest`**
  (POSIX backend — the one that fits compose, no cloud dependency) and
  `ghcr.io/transparency-dev/tesseract/gcp:latest`. `ghcr.io/sigstore/fulcio/tesseract` and
  `ghcr.io/sigstore/scaffolding/tesseract` do not exist.

Canonical stack, all refs verified pullable on this machine:

| Role | Image |
|---|---|
| Fulcio CA | `ghcr.io/sigstore/fulcio:v1.8.8` |
| CT log | `ghcr.io/transparency-dev/tesseract/posix:latest` |
| OIDC issuer | `dexidp/dex:v2.45.1` |
| Rekor v1 server | `ghcr.io/sigstore/rekor/rekor-server:v1.4.2` |
| Trillian log server / signer | `gcr.io/trillian-opensource-ci/{log_server,log_signer}:v1.7.2` (amd64-only) |
| Trillian MySQL | `gcr.io/trillian-opensource-ci/db_server:v1.4.0` (amd64-only) |

**Why the CT-log generation does not threaten the client.** The worker is right that
TesseraCT's *submission/monitoring* API (static-ct-api) differs from RFC 6962's, so a CT log
and a Fulcio version are not interchangeable across generations. But ocx never talks to the
CT log at all: Fulcio submits the precertificate, embeds the returned SCT in the issued
certificate, and ocx's only interaction is verifying that embedded SCT against the log's
public key carried in the trust root. The SCT structure itself is RFC 6962
(`crypto/transparency.rs` uses `CT_PRECERT_SCTS` / `CT_PRECERT_SIGNING_CERT` and `tls_codec`
regardless of which log minted it). So the pairing constraint binds Fulcio↔CT-log, not
ocx↔CT-log — which is what makes taking the newer pair free, and matching Fulcio's own live
compose the better default for the self-hosting documentation.

The `--ct-log-url` value must therefore point at the tesseract service, and the
`ctfe_keys` entry in the generated `trusted_root.json` must carry **tesseract's** public key
(`config/ctfe/privkey.pem`'s public half), not a legacy CTFE key.
