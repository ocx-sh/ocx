---
outline: deep
---
# Signing

You want to know whether the binary you are about to run came from the person or pipeline you trust — not just that the download arrived intact.

Checksums answer "did the file change in transit?" They do not answer "who built this?" A checksum tells you the bytes match a known digest; it cannot tell you whether an attacker replaced both the binary and the checksum file on a compromised mirror.

OCX solves this by attaching a [Sigstore][sigstore] keyless signature to each package manifest at publish time. The signature binds a cryptographic identity — a GitHub Actions workflow URL or an email address — to the exact manifest digest. At verify time, OCX checks that the identity matches what you specified and that the cryptographic proof is valid. There is no key management: the signing key is ephemeral and the certificate is issued by [Fulcio][fulcio], with an audit trail in [Rekor][rekor].

The user-facing surface — sign a release, verify what you install — lives in the [Supply-Chain Integrity section of the user guide][user-supply-chain].

Signing a published package, verifying it against a pinned identity, then verifying it again offline — against the self-hosted Sigstore stack this repository's acceptance suite runs:

<Terminal src="/casts/in-depth/signing.cast" title="Signing a package and verifying it against a pinned identity" collapsed />

## Trust Root {#trust-root}

OCX verifies [Fulcio][fulcio] certificates against a trust root, and verifies the [Rekor][rekor] Signed Entry Timestamp against Rekor's public key. Against public Sigstore this needs no configuration at all — the material arrives over [TUF][sigstore-tuf]. Against a self-hosted stack it has to come from somewhere you control, and OCX resolves it through six rungs, first hit wins:

1. **`--trusted-root <PATH>`** on `ocx package verify` — a Sigstore [trusted-root][sigstore-tuf] JSON, or a directory holding `trusted_root.json`.
2. **[`OCX_SIGSTORE_TRUSTED_ROOT`][env-sigstore-trusted-root]** — the same value as an environment variable; the flag wins.
3. **[`[trust.sigstore]`][config-trust-sigstore]** in the operator `config.toml` — `trusted_root` (a path, relative to that config file) or `trusted_root_json` (the document inlined). This is the rung a fleet uses, because a `config.toml` can itself be distributed.
4. **`$OCX_HOME/sigstore/trusted-root.json`** — a convention path. Drop the file there and nothing needs configuring.
5. **The trust-root cache** — a successful online verify writes the Fulcio CA, the CT log keys and the Rekor key it used to `$OCX_HOME/state/trust_root/`, so a later verify (including [`--offline`][env-offline]) reuses them. See [Offline and Air-Gapped Verification](#offline-verification).
6. **The public-good root over TUF** — with no override, no configured root and no cache, `TrustRoot::load_embedded` fetches and verifies the [Sigstore TUF][sigstore-tuf] trust root through [sigstore-rs][sigstore-rs]'s `tough`-backed client, caching the TUF metadata under `$OCX_HOME/state/tuf/`. This is the default for packages signed against public Sigstore, and it needs network — `--offline` never reaches it.

Rungs 1–3 are operator-named: a file that does not exist is an error, not a fall-through. Rung 4 is a convention: absent falls through to the cache, but present-and-unreadable fails. Which rung to use is a deployment decision — see [Self-hosted Sigstore][in-depth-self-hosted-sigstore] for the trade-offs.

:::warning A bare CA certificate is not a trust root
A Fulcio certificate carries an embedded Signed Certificate Timestamp, and the verifier checks that SCT against the CT log's public key. Trust material carrying CA anchors and no log key would fail on the SCT check for every real certificate, so OCX refuses it up front with exit 78 (`ConfigError`) and the message `trust root carries no CT log key`, rather than surfacing it later as a signature failure.

A Sigstore trusted-root JSON carries the anchors, the CT log keys and the pinned Rekor key together; that is the only shape any rung accepts. Produce one for a self-hosted stack with `cosign trusted-root create`, or with `test/sigstore/generate-trusted-root.py` in this repository.
:::

## Referrers Capability Cache {#referrers-cache}

[OCI Referrers][oci-referrers-spec] discovery requires the registry to implement `GET /v2/{repo}/referrers/{digest}`. OCX probes once per registry and caches the result so repeated sign or verify calls pay no extra round-trip.

Cache location: `$OCX_HOME/state/referrers/{registry_slug}.json`

The `{registry_slug}` is the registry hostname with any character outside `[a-zA-Z0-9._-]` replaced by an underscore (`_`). Dots are preserved, so `ghcr.io` stays `ghcr.io` (cache file `ghcr.io.json`); a hostname carrying a port such as `localhost:5000` becomes `localhost_5000` (the `:` is replaced).

Each cache file is a JSON object with four fields:

| Field | Type | Description |
|-------|------|-------------|
| `registry` | string | Registry hostname |
| `supported` | `"supported"` \| `"unsupported"` | Result of the last probe (snake_case) |
| `probed_at` | object `{ "secs_since_epoch", "nanos_since_epoch" }` | Wall-clock time of the probe, serialized as a serde `SystemTime` (seconds + nanoseconds since the UNIX epoch), not a bare integer |
| `ttl_seconds` | integer | Seconds after `probed_at` the entry remains valid |

The cache is advisory and fail-open: a missing or corrupt file triggers a fresh probe; the probe result then overwrites the file atomically (temp-file rename, mode `0600` on Unix). Entries are valid for **6 hours** (`TTL_SECS = 6 * 3600`); after that, the next sign or verify invocation re-probes automatically. Pass `--no-cache` to bypass the cache for a single invocation.

## OCI 1.1 Referrers Hard-Fail Policy {#referrers-hard-fail}

OCX does not implement a fallback to the [cosign][cosign] tag scheme (`sha256-<digest>.sig`). When a registry does not implement the Referrers API — the `GET /v2/{repo}/referrers/{digest}` endpoint returns HTTP 404 (or a `NOT_FOUND` / `NAME_UNKNOWN` error envelope) — the sign and verify operations fail hard with exit 84 (`ReferrersUnsupported`). Any other registry error (an auth failure, a 5xx, a transport error) surfaces under its own exit code, not as `ReferrersUnsupported`.

This is an explicit design choice: a silent fallback would let signatures be published to a registry that cannot guarantee their discoverability, or let a verification path succeed against a stale or unreachable fallback tag. Hard-fail makes the dependency on OCI 1.1 explicit so operators know exactly which registries are compatible.

:::info Which registries support OCI 1.1 Referrers?

OCX `package sign` / `package verify` require OCI Distribution Spec v1.1 Referrers API. As of May 2026:

- **Supported:** [Zot][zot], [Harbor][harbor] 2.9+, JFrog Artifactory 7.90+ (including `ocx.sh`), Amazon ECR, Azure ACR, Google Artifact Registry, Red Hat Quay 3.12+.
- **Not supported (exit 84):** CNCF Distribution `registry:2` / `registry:3` (no Referrers API — it serves only the tag-schema fallback, which OCX does not use), [GHCR][ghcr] (GitHub Container Registry), [Docker Hub][docker-hub]. Use a registry from the supported list for signed packages.

This is by design — OCX never writes legacy `sha256-<digest>.sig` fallback tags (ADR S1-F). The hard error gives operators a clear "change registry" signal rather than silent downgrade.
:::

## Sigstore Bundle Format and Storage {#bundle-storage}

A signature is a [Sigstore bundle v0.3][sigstore-bundle] — a JSON envelope carrying:

- The [Fulcio][fulcio]-issued short-lived signing certificate — the **leaf alone**, in the bundle's `verificationMaterial.certificate` field. Bundle v0.3 replaced v0.2's `x509CertificateChain` with a single leaf; the intermediates come from the trust root, so a chain would carry nothing the verifier does not already have.
- The ECDSA P-256 signature over the subject manifest's SHA-256 digest
- The [Rekor][rekor] transparency-log entry: the Signed Entry Timestamp (inclusion promise) and the Merkle inclusion proof. Both are mandatory in either direction — `ocx package sign` refuses to publish a bundle whose log entry carries no inclusion proof (exit 83), and `ocx package verify` refuses one it receives (`rekor_inclusion_proof_absent`, exit 65). The promise is only a signed statement that the entry *will* be included; the proof is the evidence that it is, in a tree whose root the log signed. Bundle profile v0.1 and v0.2 leave the proof optional at the schema level, so accepting a promise-only bundle would verify on weaker evidence than the format allows for.

OCX pushes the bundle as an OCI referrer of the subject manifest. The referrer artifact's media type is `application/vnd.dev.sigstore.bundle.v0.3+json`. The raw blob lands in `$OCX_HOME/blobs/` alongside other OCI blobs, identified by its own SHA-256 digest and referenced in the subject manifest's referrers index.

The blob is not referenced by any candidate or current symlink — it is found via the [OCI Referrers API][oci-referrers-spec] at verify time, not via the install symlink tree.

## cosign Interoperability {#cosign-interop}

OCX bundles are ordinary Sigstore bundles, and [cosign][cosign] reads and writes the same document. Two things bound how far that goes.

**cosign 3.0 or newer is required, and pre-3.0 compatibility is deliberately not offered.** Bundle v0.3's single-`certificate` profile is what cosign 3.x enforces; earlier cosign versions expect the v0.2 `x509CertificateChain` shape. OCX emits only the v0.3 shape. The verify path still *reads* both, so a bundle published by an older OCX keeps verifying.

**Discovery does not interoperate, by design.** `cosign verify` finds signatures only through its own tag schema (`sha256-<digest>.sig`); OCX publishes and reads them only through the [Referrers API][oci-referrers-spec] and writes no fallback tag ([hard-fail policy](#referrers-hard-fail)). Neither tool sees the other's signatures by pointing it at a registry reference.

What does interoperate is the bundle itself. Both directions are covered by OCX's acceptance suite, run against the same local Fulcio and Rekor:

```sh
# cosign verifies a bundle ocx produced.
cosign verify-blob \
  --bundle ocx-bundle.json \
  --trusted-root trusted_root.json \
  --certificate-identity ci@example.com \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  manifest.json
```

In the other direction, a bundle from `cosign sign-blob --bundle` pushed as a referrer of the subject manifest verifies under `ocx package verify` unchanged.

For a self-hosted Fulcio and Rekor, note that cosign 3 removed `--fulcio-url` / `--rekor-url` from its signing commands: the endpoints come from a signing config instead (`cosign signing-config create --out signing-config.json`, then `--signing-config`). `sign-blob` also needs `--trusted-root`, because it verifies the Rekor entry it just created before writing the bundle.

## Identity Matching {#identity-matching}

The certificate [Fulcio][fulcio] issues encodes the signer's identity in two fields:

- **Subject Alternative Name (SAN)** — the signer's OIDC-derived identity. For GitHub Actions this is the workflow run URL (e.g., `https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main`). For human sign flows it is an email address.
- **Fulcio OIDC issuer extension** — the OID `1.3.6.1.4.1.57264.1.1` contains the OIDC issuer URL (e.g., `https://token.actions.githubusercontent.com`).

At verify time, the accepted SAN and issuer come from one of two sources. Passed as `--certificate-identity` / `--certificate-oidc-issuer` flags, both checks are exact-match. Resolved instead from a [`[[trust.policy]]`][config-trust] entry whose scope covers the target, the SAN check additionally accepts an anchored regex form (`identity_regexp`); the issuer check stays exact-match either way. See the [configuration reference][config-trust] for the full schema, scope-matching rules, and the tier-pooling behavior.

A concrete GitHub Actions identity looks like this:

```
--certificate-identity https://github.com/<org>/<repo>/.github/workflows/<file>.yml@refs/heads/main
--certificate-oidc-issuer https://token.actions.githubusercontent.com
```

The `@refs/heads/main` suffix is the ref the workflow ran on; pin to the exact ref you publish from. The `<file>.yml` is the path inside `.github/workflows/` of the workflow file that signed.

## Choosing a Sigstore Deployment {#deployments}

Two deployments, one command surface. What changes between them is where the trust material comes from, and whether the endpoint flags appear at all.

### Public Sigstore {#public-good}

The default, and the case that needs no configuration. [Fulcio][fulcio] and [Rekor][rekor] run as the [Sigstore public-good instance][sigstore-public-good]; the trust root arrives over [TUF][sigstore-tuf] on first use and caches under `$OCX_HOME/state/tuf/`. Neither `--fulcio-url` nor `--rekor-url` is passed, and neither is `--trusted-root`:

```sh
ocx package sign -p linux/amd64 registry.example.com/acme/mytool:1.0.0
ocx package verify -p linux/amd64 registry.example.com/acme/mytool:1.0.0 \
  --certificate-identity you@example.com \
  --certificate-oidc-issuer https://github.com/login/oauth
```

Public Sigstore writes every certificate it issues, and every entry it logs, to a world-readable transparency log. That is the property the whole model rests on — and it is also why a private artifact often wants the self-hosted path below: the *identity* that signed an internal tool, and the times it was signed, are otherwise public even though the artifact is not.

### Self-hosted Sigstore {#self-hosted}

Fulcio and Rekor plus what they depend on: a certificate-transparency log, [Trillian][trillian] behind it, and an OIDC provider to mint identities. This repository runs exactly that as the acceptance suite's fixture — seven services in `test/docker-compose.yml` under the `sigstore` profile ([dex][dex], Fulcio, Rekor, [TesseraCT][tesseract], two Trillian services, MySQL). It is what the recording at the top of this page talks to, and `test/sigstore/README.md` documents it.

```sh
cd test && docker compose --profile sigstore up -d
```

Three things change relative to public Sigstore. The endpoints are addressed with `--fulcio-url` and `--rekor-url` — **flags only**, no environment variable for either. The trust root has to be produced once, because a self-hosted CA is in no TUF root:

```sh
cosign trusted-root create --certificate-chain fulcio-ca.crt.pem --out trusted_root.json
python3 test/sigstore/generate-trusted-root.py    # what this repo uses
```

And the identity comes from *your* issuer rather than `oauth2.sigstore.dev` — which is the part that decides whether the stack can run air-gapped at all.

`validate_sigstore_url` accepts `http://` for a loopback endpoint only; a self-hosted stack on any other host must be `https://`, and its address must clear the SSRF floor — see [Custom Sigstore endpoints](#offline-verification) at the end of this page.

#### Pin the identity once {#self-hosted-policy}

Passing `--certificate-identity` and `--certificate-oidc-issuer` on every verify is how the flags work, not how a deployment should be run. A [`[[trust.policy]]`][config-trust] entry in the operator `config.toml` states the pin once, and `ocx package verify` then needs neither flag:

```toml
# $OCX_HOME/config.toml
[[trust.policy]]
scope = "acme/mytool"
identity = "ocx-test@example.com"
oidc_issuer = "http://dex:5556/dex"
```

The issuer here is the address **inside** the compose network, not the one the host dials — the token's `iss` claim names the URL its issuer answered at when Fulcio validated it. OCX only ever compares that string; it never dials the issuer, so the two do not have to agree.

Once the policy is in place it is also what makes verification automatic on `install` and `pull` — see [Verify by default][guide-auto-verify].

**Running this for a fleet is its own page.** Which OIDC issuer to point Fulcio at (GitHub Actions, GitHub Enterprise Server, GitLab, generic OIDC), what each one costs in egress, how the trusted root reaches every machine without setting an environment variable on each, and why `identity_regexp` is an authorization boundary rather than a convenience — all of it is in [Self-hosted Sigstore][in-depth-self-hosted-sigstore].

## Signing from CI {#ci}

CI is the case keyless signing exists for: the runner already holds an OIDC identity, so there is no key to store and none to rotate. OCX finds that identity on its own — the [token precedence order][cmd-package-sign-token-precedence] reaches ambient CI detection well before it would consider opening a browser — so a signing step is one line with no credential wiring.

The identity is issued *to* the pipeline, not chosen by it. Run the signing step once and read `Certificate identity` and `Certificate OIDC issuer` off the output: those two strings, exactly, are what a `[[trust.policy]]` or a `--certificate-identity` pair has to match.

::: code-group

```yaml [GitHub Actions]
permissions:
  contents: read
  id-token: write          # required — this is what provisions the OIDC token to detect

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: ocx-sh/setup-ocx@<sha> # v1.2.2
      - run: ocx package push registry.example.com/acme/mytool:1.0.0 mytool.tar.xz
      - run: ocx package sign -p linux/amd64 registry.example.com/acme/mytool:1.0.0
```

```yaml [GitLab CI/CD]
publish:
  id_tokens:
    SIGSTORE_ID_TOKEN:     # the name matters — this is the variable ocx reads
      aud: sigstore
  script:
    - ocx package push registry.example.com/acme/mytool:1.0.0 mytool.tar.xz
    - ocx package sign -p linux/amd64 registry.example.com/acme/mytool:1.0.0
```

:::

GitHub Actions is detected through `ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, which is what [`id-token: write`][gha-oidc] provisions; GitLab through [`id_tokens`][gitlab-id-tokens] under the name `SIGSTORE_ID_TOKEN`; CircleCI through `CIRCLE_OIDC_TOKEN_V2`. Any other provider hands the token over explicitly — [`OCX_IDENTITY_TOKEN`][env-identity-token] or `--identity-token-file` — which is the same shape a self-hosted stack uses, and the shape the recording above runs under.

The consuming side is a verify step, and it wants the pin rather than the flags:

```yaml
- run: ocx package verify -p linux/amd64 registry.example.com/acme/mytool:1.0.0
  env:
    # Self-hosted only — public Sigstore needs no trust-root override, and a
    # fleet should get this from configuration rather than a per-job env var.
    OCX_SIGSTORE_TRUSTED_ROOT: ${{ github.workspace }}/trust/trusted_root.json
```

Broader CI wiring — toolchain-tier installs, environment export, the GitLab equivalents — is in [CI Integration][guide-ci]. Pointing a **private** Fulcio at these same issuers, instead of public Sigstore, is [Self-hosted Sigstore][in-depth-self-hosted-sigstore].

## Slice Boundary {#slice-boundary}

**This release** wires the complete keyless pipeline: OIDC token acquisition, ephemeral ECDSA P-256 keypair generation, the [Fulcio][fulcio] certificate request, the [Rekor][rekor] log entry, [Sigstore bundle v0.3][sigstore-bundle] assembly, the referrer push, and the full five-check verify path — certificate chain against the trust root, Rekor SET, signature over the subject digest, identity match, issuer match. Sign and verify run end-to-end; their exit-code and flag contracts are stable.

The cryptography is [sigstore-rs][sigstore-rs]'s, not OCX's. Certificate-chain building, SCT verification, the ECDSA signature check, the transparency-log body binding and the certificate-validity-versus-integrated-time check all run inside `sigstore::bundle::verify::Verifier`. OCX owns the Rekor Signed Entry Timestamp and the Merkle inclusion proof, which sigstore-rs 0.14 leaves unimplemented — and those are computed with sigstore-rs's own primitives. No X.509, ASN.1, Merkle, SCT or signature code is hand-written anywhere in OCX.

The acceptance suite runs against a real Sigstore deployment — Fulcio, Rekor, TesseraCT and dex under the `sigstore` Docker Compose profile (`test/sigstore/README.md`) — not a fake. Hostile artifacts are made the way an attacker would make them: take a genuine signed bundle off the registry, change one field, put it back.

## Current Limitations {#current-limitations}

- **Rekor v1 only.** [sigstore-rs][sigstore-rs] 0.14 ships no Rekor v2 (tiles) client, so OCX targets Rekor v1 `hashedrekord`. A bundle from a Rekor v2 instance carries an RFC 3161 TSA timestamp instead of a SET; OCX rejects it with exit 83 (`RekorUnavailable`) rather than treating it as unsigned. Tracked as [#107][gh-107].

  This is a dated dependency, not a static gap: it holds only while the log you sign against speaks v1. The moment an instance — the public-good deployment or your own — serves v2, every **new** signature it issues verifies as exit 83 here, and signatures already in a v1 log keep verifying. Treat a planned Rekor upgrade as a blocking prerequisite on [#107][gh-107], and pin the Rekor URL you verify against rather than following an instance through a migration.
- **No DSSE attestations.** `ocx package attest` does not exist, and the verify path rejects a DSSE-envelope bundle with exit 79 (`NoUsableBundle`). Deferred until sigstore-rs ships DSSE support.
- **Discovery does not interoperate with cosign** — see [cosign Interoperability](#cosign-interop).

:::tip Automatic verification at install time
Everything above describes the standalone `ocx package verify` command. Once a [`[[trust.policy]]`][config-trust] covers a package, [`install`][cmd-package-install], [`pull`][cmd-package-pull], and every command that auto-installs on demand run the same check automatically — see [Verify by default][guide-auto-verify] in the user guide. That gate has its own scope limitations, distinct from the cryptographic ones above: a covered root's transitive dependencies are verified only if a policy also covers each dependency's own `registry/repository` scope, and the automatic check reads the operator `config.toml` tier only — a project `ocx.toml` policy never gates it.
:::

## Deferred to Future Work {#deferred-future-work}

- **Rekor v2** ([#107][gh-107]) — a tiles-based transparency log with RFC 3161 timestamps in place of the SET. Blocked on a Rekor v2 client in [sigstore-rs][sigstore-rs].
- **DSSE attestations** — `ocx package attest`, and verification of DSSE-envelope bundles. Blocked on DSSE support in sigstore-rs.

## Offline and Air-Gapped Verification {#offline-verification}

Verifying an artifact means reading it — and its signature — from the registry where it lives. In an air-gapped deployment that registry is a local mirror the operator runs, so `ocx package verify` treats the artifact registry as always-available. What `--offline` / [`OCX_OFFLINE`][env-offline] removes for verify is the **Sigstore trust-services** network: the Rekor public-key fetch and TUF. Those are the calls that need trust material, and offline verify sources that material locally instead.

There are two offline paths:

- **Supplied trust root.** Any of the first four rungs of the [trust-root ladder](#trust-root) — `--trusted-root`, [`OCX_SIGSTORE_TRUSTED_ROOT`][env-sigstore-trusted-root], [`[trust.sigstore]`][config-trust-sigstore], or `$OCX_HOME/sigstore/trusted-root.json`. A Sigstore trusted-root JSON carries the Fulcio CA, the CT log keys and the pinned Rekor public key together, so the SET verifies with no fetch. This is the air-gapped seam: point it at a local trust-root mirror, or ship it through configuration — see [Self-hosted Sigstore][in-depth-self-hosted-sigstore].
- **Cached trust root.** A successful **online** `ocx package verify` writes the Fulcio CA and the Rekor key it used to `$OCX_HOME/state/trust_root/<rekor-authority>.json` (24-hour TTL). A later `--offline` verify against the same Rekor instance reuses that cache with no fetch.

Offline verify requires a **pinned Rekor key**, which is why the trust material has to be a full trusted-root JSON and not a bare CA certificate. When no cached or supplied trust material is available offline, verify fails with exit 78 (`ConfigError`) naming the remedy; it never silently skips verification.

```sh
# Air-gapped: pin both the Fulcio CA and the Rekor key from a local mirror.
ocx --offline package verify -p linux/amd64 registry.internal/cmake:3.28 \
  --trusted-root /etc/ocx/trusted_root.json \
  --certificate-identity ci@example.com \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

`ocx package sign` stays online-only — it needs live Fulcio and Rekor round-trips — and rejects `--offline` with exit 77 (`PermissionDenied`), a policy on the action distinct from verify's read-side behavior.

:::tip Custom Sigstore endpoints
`--fulcio-url` and `--rekor-url` point the CLI at a private or self-hosted Sigstore deployment instead of the public Fulcio/Rekor. `validate_sigstore_url` accepts `http://` only for loopback hosts (`127.0.0.0/8`, `::1`, `localhost`); any non-loopback target must be `https://`.

The same SSRF floor that guards registry traffic guards these endpoints: a URL is refused by **where it resolves**, not by how it is spelled, so a private-range or link-local address is rejected with exit 64 (`InvalidEndpointUrl`), the same code as any other malformed `--fulcio-url`/`--rekor-url`. A self-hosted Sigstore on a private network is therefore reachable only after its host is allow-listed — `[registries."<ns>"] trusted_hosts` in the operator `config.toml`, the same key that admits a private registry.
:::

## Signing Flow Summary {#signing-flow}

1. OCX resolves the OIDC identity token using the [token precedence order][cmd-package-sign-token-precedence]:
   `--identity-token-file` → `--identity-token-stdin` → [`OCX_IDENTITY_TOKEN`][env-identity-token]
   → ambient CI detection → interactive browser OAuth.
2. An ephemeral ECDSA P-256 keypair is generated in memory.
3. The ephemeral public key is sent to [Fulcio][fulcio] with the OIDC token; Fulcio issues a short-lived certificate binding the key to the OIDC identity.
4. The subject manifest's SHA-256 digest is signed with the ephemeral private key. The key is zeroized immediately after signing.
5. The log entry is posted to [Rekor][rekor]; the response contains the SET and the Merkle inclusion proof. A log that returns no usable proof fails the sign with exit 83 rather than publishing a bundle OCX itself would refuse to verify.
6. The leaf certificate, the signature, and the log entry (SET plus inclusion proof) are assembled into a [Sigstore bundle v0.3][sigstore-bundle] and pushed to the registry as a referrer of the subject manifest.

## See Also {#see-also}

- [`package sign` reference][cmd-package-sign] — flags, token-source precedence, exit codes, CI example
- [`package verify` reference][cmd-package-verify] — flags, identity matching options, exit codes
- [Configuration reference → `[[trust.policy]]`][config-trust] — schema, scope matching, most-specific-wins resolution, operator-vs-project tier precedence
- [cosign Interoperability](#cosign-interop) — the cosign 3.0 floor and what does and does not interoperate
- [Deferred to Future Work](#deferred-future-work) — Rekor v2 ([#107][gh-107]) and DSSE attestations
<!-- external -->
[sigstore]: https://www.sigstore.dev/
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
[cosign]: https://github.com/sigstore/cosign
[sigstore-bundle]: https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_bundle.proto
[sigstore-tuf]: https://docs.sigstore.dev/certificate_authority/overview/
[sigstore-rs]: https://github.com/sigstore/sigstore-rs
[oci-referrers-spec]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#listing-referrers
[ghcr]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry
[docker-hub]: https://hub.docker.com/
[ecr]: https://aws.amazon.com/ecr/
[acr]: https://azure.microsoft.com/en-us/products/container-registry
[harbor]: https://goharbor.io/
[zot]: https://zotregistry.dev/
[registry-v2]: https://distribution.github.io/distribution/
[sigstore-public-good]: https://docs.sigstore.dev/about/public-deployment/
[trillian]: https://github.com/google/trillian
[tesseract]: https://github.com/transparency-dev/tesseract
[dex]: https://dexidp.io/
[gha-oidc]: https://docs.github.com/en/actions/deployment/security-hardening-your-deployments/about-security-hardening-with-openid-connect
[gitlab-id-tokens]: https://docs.gitlab.com/ee/ci/yaml/#id_tokens

<!-- commands -->
[cmd-package-sign]: ../reference/command-line.md#package-sign
[cmd-package-sign-token-precedence]: ../reference/command-line.md#package-sign
[cmd-package-verify]: ../reference/command-line.md#package-verify
[cmd-package-install]: ../reference/command-line.md#package-install
[cmd-package-pull]: ../reference/command-line.md#package-pull

<!-- reference -->
[config-trust]: ../reference/configuration.md#keys-trust

<!-- issues -->
[gh-107]: https://github.com/ocx-sh/ocx/issues/107

<!-- environment -->
[env-identity-token]: ../reference/environment.md#ocx-identity-token
[env-sigstore-trusted-root]: ../reference/environment.md#ocx-sigstore-trusted-root
[config-trust-sigstore]: ../reference/configuration.md#keys-trust-sigstore
[in-depth-self-hosted-sigstore]: ./self-hosted-sigstore.md
[env-offline]: ../reference/environment.md#ocx-offline

<!-- user guide -->
[user-supply-chain]: ../user-guide.md#supply-chain
[guide-auto-verify]: ../user-guide.md#supply-chain-auto-verify
[guide-ci]: ./ci.md
