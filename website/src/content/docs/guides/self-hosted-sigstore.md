---
title: "Self-hosted Sigstore"
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

Use this page to sign and verify internal artifacts against your own [Fulcio][fulcio] and [Rekor][rekor] instead of [public Sigstore][sigstore-public-good]. It also shows how to get the trusted root onto every machine without a per-host step.

Public Sigstore puts the *identity* of every signer, and the time of every signature, into a world-readable transparency log. That happens even though the artifact never leaves your network. Your own stack keeps both inside.

The hard part is the trust root. A self-hosted CA appears in no TUF root, so every machine that verifies has to be told what to trust. This page covers four things:

- the components to run
- the trusted root and how it reaches every machine
- the OIDC issuer to point Fulcio at, including GitHub Actions
- an end-to-end check

The mechanics of signing and verifying are in [Signing][in-depth-signing].

## What you have to run {#components}

Four components. Only the first two are OCX's concern. The other two are what those two depend on.

| Component | What it does | Do you have a choice? |
|---|---|---|
| [Fulcio][fulcio] | Exchanges an OIDC token for a short-lived X.509 certificate whose SAN *is* the caller's identity. This is the CA. | No, OCX speaks Fulcio |
| [Rekor][rekor] | Append-only transparency log. Records a signature or an attestation and returns a Signed Entry Timestamp that proves *when* it was logged. Entries are `hashedrekord` for signatures and `dsse` for attestations. | No, v1 only, see [Current limitations][signing-limitations] |
| A certificate-transparency log | Fulcio embeds a Signed Certificate Timestamp in every certificate it issues, and the verifier checks it. No CT log means no verifiable certificates. | [TesseraCT][tesseract] or a Trillian-backed CTFE |
| An OIDC issuer | Mints the token Fulcio trusts. **This decision determines whether the stack can run air-gapped.** See [the issuer matrix](#issuers). | Yes, and it matters |

This repository runs exactly that as its acceptance fixture. It is seven services in `test/docker-compose.yml` under the `sigstore` profile: [dex][dex], Fulcio, Rekor, [TesseraCT][tesseract], two Trillian services and MySQL. `test/sigstore/README.md` documents the wiring, and it is a working reference for a small internal deployment.

```sh
cd test && docker compose --profile sigstore up -d
```

## Where the trusted root comes from {#trusted-root}

A *trusted root* is one JSON document carrying three things that must travel together:

- the **Fulcio CA certificate(s)**, the anchor the leaf certificate chains to
- the **certificate-transparency log public keys**, without which the embedded SCT cannot be checked (OCX refuses trust material that lacks them with exit `78`)
- the **Rekor public key**, pinned so the Signed Entry Timestamp verifies with no call to Rekor (this is what makes [`--offline`][arg-offline] verification possible)

A bare CA certificate is *not* a trust root. It is the most natural thing to reach for, and it verifies nothing.

Produce the trusted root once, from the stack you just stood up. Fetch the three inputs, then hand them to `cosign trusted-root create`. Fulcio serves its CA chain at `/api/v1/rootCert` and Rekor serves its public key at `/api/v1/log/publicKey`. The CT log serves no such endpoint here. Take the public half of the signing key you configured the log with, as a PEM file named `ctfe.pub`. With an EC signing key in `ct.key.pem` on the log host, `openssl ec -in ct.key.pem -pubout -out ctfe.pub` writes it. Copy `ctfe.pub` to the machine that runs the next step:

```sh
curl -fsS https://fulcio.corp.example/api/v1/rootCert -o fulcio-ca.crt.pem
curl -fsS https://rekor.corp.example/api/v1/log/publicKey -o rekor.pub
cosign trusted-root create \
  --fulcio="url=https://fulcio.corp.example,certificate-chain=fulcio-ca.crt.pem" \
  --rekor="url=https://rekor.corp.example,public-key=rekor.pub,start-time=2024-01-01T00:00:00Z" \
  --ctfe="url=https://ctfe.corp.example,public-key=ctfe.pub,start-time=2024-01-01T00:00:00Z" \
  --no-default-fulcio --no-default-rekor --no-default-ctfe --no-default-tsa \
  --out trusted_root.json
```

```sh
python3 test/sigstore/generate-trusted-root.py    # what this repository uses for its fixture
```

Regenerate it when the CA rotates or a CT log is added. It is public material with no secret in it. You can commit it, publish it to a registry, or serve it over plain HTTP inside your network without care.

### The `[trust.sigstore]` fields {#trusted-root-fields}

One sub-table of `config.toml` sets the trusted root and the endpoints `ocx package sign` talks to by default. Every field is optional, and the whole sub-table may be absent. Omitting it reproduces public-good behaviour exactly.

| Field | Type | Description |
|-------|------|-------------|
| `trusted_root` | string | A bare path, or a `file://` one, naming a trusted-root JSON or a directory holding `trusted_root.json`. A relative path resolves against the directory of the `config.toml` that declared it, rewritten to absolute at load time, so it means the same file whatever the working directory. Mutually exclusive with `trusted_root_json` |
| `trusted_root_json` | string | The trusted-root document inlined verbatim. This is the form a fleet receives. Mutually exclusive with `trusted_root` |
| `fulcio_url` | string | Default [Fulcio][fulcio] base URL for `ocx package sign` and `attest` when `--fulcio-url` is omitted: an explicit flag wins, then this field, then the public-good builtin. `ocx package push` takes the same `--fulcio-url`, so this field is its default too |
| `rekor_url` | string | Default [Rekor][rekor] base URL for `sign`, `verify`, `attest` and `sbom` when `--rekor-url` is omitted, with the same precedence. `ocx package push` takes the same `--rekor-url`. Auto-verify exposes no such flag, so this field is its only override |
| `rekor_upload` | boolean | The fleet-wide default for uploading a **key-mode** signature to the transparency log. Absent means off |

Setting both `trusted_root` and `trusted_root_json` is a configuration error: exit 78, `trust_root_load`. One trust root, one spelling.

`rekor_upload` governs key mode only. Under keyless signing the upload is a requirement. A Fulcio certificate is valid for about ten minutes, and the Rekor timestamp is the only proof the signature happened inside that window. The field is silently ignored for a keyless signer, deliberately without a warning. `rekor_url` defaults to the public Rekor, so enabling `rekor_upload` there publishes the artifact's digest and signer to a world-readable log.

OCX reads the sub-table from the `config.toml` tiers only. The project `ocx.toml` parses a `[trust]` section for `[[trust.policy]]`, but its `sigstore` sub-table is never consulted. A repository that could name its own Fulcio CA would be verifying its own signatures, which is the entire trust decision.

**Locking.** Declared at the system scope, the whole sub-table is non-overridable. The user, `$OCX_HOME` and managed tiers cannot replace any field. The lock is per table rather than per field, so a lower tier cannot supply a `rekor_url` alongside a system `trusted_root`.

A scalar trust root cannot pool, because two Fulcio CAs are an ambiguity and not a merge. That is why this follows the `[registry]` lock rather than the `[[trust.policy]]` one.

Where the table is *not* locked, higher tiers replace field by field, with the two trust-root spellings coupled. A tier switching from a path to an inline document drops the path rather than leaving both set.

## Getting it onto every machine {#distribution}

OCX resolves the trust root through [six rungs](../explanation/signing.mdx#trust-root), and the first hit wins. Four of them are yours to choose between. They are listed here cheapest-first in operator effort, which is the inverse of how most people discover them.

### 1. Ship one `config.toml` to the fleet {#distribution-managed}

This is the one that scales. OCX reads `[trust.sigstore]` from the operator `config.toml` tiers, and the [`[managed]`][config-managed] tier makes one published `config.toml` the fleet's configuration. Put the trusted root in the payload:

```toml
# the config.toml you publish
[trust.sigstore]
trusted_root = "trusted_root.json"   # path, relative to THIS file
```

```sh
ocx config push -i registry.corp.example/ops/ocx-config:1 ./fleet-config.toml
```

[`config push`][cmd-config-push] takes one config file, so `trusted_root.json` has to sit beside it. That relative path resolves against the declaring file's own directory. At publish time `config push` reads the file and inlines it as `trusted_root_json`. What the fleet receives is self-contained and names no path on anyone's disk. Consumers adopt the seed once:

```sh
ocx self setup --managed-config registry.corp.example/ops/ocx-config@sha256:<digest>
```

:::caution[Seed the managed tier by digest]
A payload carrying `trusted_root_json` is **ignored** unless the `[managed]` seed is digest-pinned. Otherwise the trust root would arrive over the very channel it exists to verify. A pinned seed breaks that circularity, not policy. Path-form `trusted_root` arriving from the managed tier is likewise ignored, with a warning, because a fleet payload cannot name a path on someone else's machine.
:::

`fulcio_url` and `rekor_url` arriving from a `[managed]` source that is not digest-pinned are ignored with a warning too, for the same reason. `fulcio_url` is the sharper case. It names where the OIDC identity token is sent. A `--fulcio-url` on `ocx package push` overrides the configured value, but without a flag an unpinned payload could hand a signing identity to a server of its choosing.

Rotation from there is [`ocx config update`][cmd-config-update] against a new digest. A cascade tag works too, if you seeded by tag *and* keep the trust root out of the payload.

### 2. Write it into `/etc/ocx/config.toml` {#distribution-system}

If you already own the machine image or run a configuration-management tool, the system-scope config is the shortest path. It is also the **strongest** one. A `[trust.sigstore]` declared at system scope is locked, and no user, home, or managed tier can replace it.

```toml
# /etc/ocx/config.toml
[trust.sigstore]
trusted_root = "sigstore/trusted-root.json"   # relative to /etc/ocx/
```

A relative path anchors to the declaring file's own directory, so the same line means the same file regardless of anyone's working directory.

### 3. Drop the file at the convention path {#distribution-convention}

This needs no configuration at all. Put the document where OCX already looks:

```sh
install -Dm644 trusted_root.json "$OCX_HOME/sigstore/trusted-root.json"
```

Absent, it falls through to the next rung. Present but unreadable, it fails. That is deliberate, because a trust root you meant to install and cannot read is not the same thing as one you never installed.

### 4. `--sigstore-trusted-root` / `OCX_SIGSTORE_TRUSTED_ROOT` {#distribution-flag}

The flag and its environment variable are for one-off overrides. Examples are a debugging session, a CI job pinning a specific root, or a machine that is not part of the fleet. Reaching for them as the *deployment* mechanism is how you end up setting an environment variable in every shell profile on every host.

OCX reads the override as-is. It bypasses the TUF fetch that runs when no override, no configured trust root and no cache exist. Keeping the supplied document current is therefore the operator's job. The override affects only the local verify operation and is not forwarded to subprocess children.

## Custom endpoints {#endpoints}

`--fulcio-url` and `--rekor-url` point the CLI at a private or self-hosted Sigstore deployment instead of the public Fulcio and Rekor. They are **flags only**. Neither has an environment variable. Config defaults for both are the `fulcio_url` and `rekor_url` fields above.

`validate_sigstore_url` accepts `http://` only for loopback hosts: `127.0.0.0/8`, `::1` and `localhost`. Any non-loopback target must be `https://`.

The same SSRF floor that guards registry traffic guards these endpoints. A URL is refused by **where it resolves**, not by how it is spelled. An endpoint that resolves into a forbidden range is rejected with exit 64 (`InvalidEndpointUrl`), the same code as any other malformed `--fulcio-url` or `--rekor-url`. A private-range or link-local address with no `trusted_hosts` entry is such a range. An endpoint that does not resolve at all exits 69 (`Unavailable`), and a rerun changes nothing until the host or the network is fixed.

An endpoint that fails to resolve at all exits 69 instead. A rerun changes nothing until the host or the network is fixed.

A self-hosted Sigstore on a private network is therefore reachable only after its host is allow-listed. Use `[registries."<ns>"] trusted_hosts` in the operator `config.toml`, the same key that admits a private registry.

Under a configured [HTTP proxy][env-external-proxies], a hostname-configured `--fulcio-url` or `--rekor-url` is dialed through the proxy rather than refused. OCX itself never resolves the destination when a proxy is in the path. The guard admits the proxy's own hostname at the connection-time resolver instead of treating it as an unapproved host.

## The issuer matrix {#issuers}

Fulcio does not mint identities. It validates tokens from an issuer you name in its own `config.yaml` and turns them into certificates. Which issuer you pick decides how much of the stack can live inside your network.

| Setup | Air-gappable? | What it needs |
|---|---|---|
| GitHub.com Actions + GitHub-hosted runner | **No** | The runner cannot reach an internal registry at all. This is a network problem, not a Sigstore one |
| GitHub.com Actions + self-hosted runner | Nearly | One egress allowlist to `token.actions.githubusercontent.com`, so Fulcio can fetch the issuer's JWKS |
| GitHub Enterprise Server + self-hosted runner | **Yes** | Fully internal |
| GitLab self-managed | **Yes** | Fully internal |
| Generic OIDC ([dex][dex], Keycloak) + any runner | **Yes** | Fully internal; the only path with live acceptance coverage here, see [Coverage](#coverage) |

The egress in row 2 is Fulcio's, not the runner's. Fulcio validates the token's signature against the issuer's published keys, so it must reach the issuer's JWKS endpoint. Nothing else in the pipeline leaves the network.

### Fulcio configuration per issuer {#issuers-fulcio}

Each issuer is one entry in Fulcio's `config.yaml`. The `type` selects how Fulcio derives the certificate SAN from the token's claims. The values below are the ones upstream Fulcio defines in `config/identity/config.yaml`:

```yaml
# GitHub.com Actions
oidc-issuers:
  https://token.actions.githubusercontent.com:
    issuer-url: https://token.actions.githubusercontent.com
    client-id: sigstore
    type: github-workflow
```

```yaml
# GitHub Enterprise Server: read the exact issuer URL off a real token (below)
oidc-issuers:
  https://ghe.corp.example/_services/token:
    issuer-url: https://ghe.corp.example/_services/token
    client-id: sigstore
    type: github-workflow
```

```yaml
# GitLab self-managed
oidc-issuers:
  https://gitlab.corp.example:
    issuer-url: https://gitlab.corp.example
    client-id: sigstore
    type: gitlab-pipeline
```

Fulcio ships further types, each deriving the SAN from that platform's claims: `email`, `kubernetes`, `circleci-job`, `buildkite-job`, `codefresh-workflow`, `buddy-pipeline` and `chainguard-identity`. Consult upstream's `config/identity/config.yaml` for the current set rather than this page. It is Fulcio's list, not OCX's.

### Read the identity, do not guess it {#issuers-read}

Every worked example below is the *expected shape*. The strings that matter are whatever your issuer put in the token and Fulcio put in the certificate. They differ by platform version and by how a job was triggered. Sign once and read them off the output:

```sh
ocx package sign -p linux/amd64 \
  --fulcio-url https://fulcio.corp.example \
  --rekor-url https://rekor.corp.example \
  registry.corp.example/acme/mytool:1.0.0
```

`Certificate identity` and `Certificate OIDC issuer` in that output are, byte for byte, what a policy has to match. Copy them and do not retype them.

### Worked policies {#issuers-policies}

**GitHub Actions** (either github.com or GHES, where only the host differs). The identity is the workflow file at a ref, not a person:

```toml
# $OCX_HOME/config.toml, or the config.toml you publish to the fleet
[[trust.policy]]
scope = "registry.corp.example/acme"

signers = [
  { kind = "keyless", identity = "https://github.com/acme/mytool/.github/workflows/release.yml@refs/tags/v1.0.0",
                      oidc_issuer = "https://token.actions.githubusercontent.com" },
]
```

Pinning an exact tag ref means every release needs a policy edit. The usual shape is a regexp over the ref, with the workflow path fixed:

```toml
[[trust.policy]]
scope = "registry.corp.example/acme"

signers = [
  { kind = "keyless", identity_regexp = "https://github\\.com/acme/mytool/\\.github/workflows/release\\.yml@refs/tags/v.*",
                      oidc_issuer = "https://token.actions.githubusercontent.com" },
]
```

**GitLab self-managed.** The identity is the CI config file at a ref. Note GitLab's doubled slash between project path and config path:

```toml
[[trust.policy]]
scope = "registry.corp.example/acme"

signers = [
  { kind = "keyless", identity_regexp = "https://gitlab\\.corp\\.example/acme/mytool//\\.gitlab-ci\\.yml@refs/tags/v.*",
                      oidc_issuer = "https://gitlab.corp.example" },
]
```

**Generic OIDC.** A human or a service account, identified by email:

```toml
[[trust.policy]]
scope = "registry.corp.example/acme"

signers = [
  { kind = "keyless", identity = "release-bot@corp.example",
                      oidc_issuer = "https://sso.corp.example" },
]
```

**Compose fixture.** The acceptance suite signs against a local Fulcio and a dex issuer. Dex mints a fixed email identity, and its `iss` claim is the in-network URL:

```toml
[[trust.policy]]
scope = "localhost:5000/acme"

signers = [
  { kind = "keyless", identity = "ocx-test@example.com",
                      oidc_issuer = "http://dex:5556/dex" },
]
```

OCX always compares `oidc_issuer` byte-for-byte and never dials it, so it does not have to be an address the verifying machine can reach. In a compose fixture, the token's `iss` claim names the URL its issuer answered at. That is the address inside the compose network, not the one the host dials.

OCX wraps `identity_regexp` as `\A(?:…)\z` before compiling it, so it must match the **entire** SAN. You never write the anchors yourself. A pattern that looks like a substring match is a full match.

Once a policy covers a package, it also makes verification automatic on `install` and `pull`. See [Verify by default][guide-auto-verify].

## `identity_regexp` is an authorization boundary {#authorization}

A regexp that matches more than you meant is not a slightly loose config. It is a grant.

```toml
# Every project on the instance. Anyone who can create one can sign your packages.
{ kind = "keyless", identity_regexp = "https://gitlab\\.corp\\.example/.*", oidc_issuer = "..." }
```

Under that policy, an engineer who creates `gitlab.corp.example/anyone/scratch` and pushes a `.gitlab-ci.yml` gets a certificate that satisfies it. The signature is genuine, Rekor logged it, and OCX accepts it, because you said that identity was trusted.

Three rules keep the boundary real:

1. **Anchor everything you can.** Fix the project path and the workflow or CI-config path, and leave the wildcard on the ref alone. `…/acme/mytool//\.gitlab-ci\.yml@refs/tags/v.*` is a grant to one file in one project. `…/acme/.*` is a grant to a namespace, which is only as tight as who can create projects in it.
2. **Escape the dots.** `gitlab.corp.example` as a regexp matches `gitlabXcorpYexample`. Every literal `.` in a host or a filename is `\.`.
3. **The CI side is half the control.** A policy pinning `release.yml@refs/tags/v*` is only as strong as the tag-push and file-edit rights behind it. Pair it with protected tags (or protected branches) and CODEOWNERS on the workflow or CI-config file. Also limit who can create projects in the namespace the scope covers. OCX enforces *which identity signed*, and your forge enforces *who can become that identity*.

For scopes where the answer must not be negotiable, declare the policy at system scope (`/etc/ocx/config.toml`). A system-scope policy is admission-authoritative. No user, project, or managed-tier policy can outbid it with a narrower scope or join its accepted set. The full resolution model is in [Identity Matching][signing-identity-matching] and [`[[trust.policy]]`][config-trust].

## Verifying the setup end to end {#verify-setup}

Four steps, and the fourth is the one that matters. A check you have never seen fail is not a check.

**1. Sign.** The signer needs an OIDC token. It comes from `--identity-token-file`, `--identity-token-stdin`, `OCX_IDENTITY_TOKEN`, ambient CI detection or interactive browser OAuth, in that order. See [`ocx package sign`][cmd-package-sign] for the precedence.

```sh
ocx package sign -p linux/amd64 \
  --fulcio-url https://fulcio.corp.example \
  --rekor-url https://rekor.corp.example \
  registry.corp.example/acme/mytool:1.0.0
```

**2. Verify on a machine that has only the fleet configuration**, with no flag and no environment variable:

```sh
ocx package verify -p linux/amd64 registry.corp.example/acme/mytool:1.0.0
```

Exit `0` with no `--sigstore-trusted-root` and no `--certificate-identity` proves both halves at once. The trust root arrived through configuration, and a `[[trust.policy]]` matched.

**3. Verify offline**, to prove the pinned Rekor key is really in the trust material:

```sh
ocx package verify --offline -p linux/amd64 registry.corp.example/acme/mytool:1.0.0
```

**4. Break it on purpose.** Each of these must fail, with the exit code named:

| Break | Expected |
|---|---|
| Edit `identity` in the policy to a wrong-but-plausible value | exit `77`, identity mismatch |
| Point `oidc_issuer` at a different issuer | exit `77`, issuer mismatch |
| Remove the trust root from every rung and run `--offline` | exit `81`, slug `offline_mode`, no pinned Rekor key |
| Strip the `ctlogs` entries out of the trusted root | exit `78`, `trust root carries no CT log key` |
| Sign from a project the policy does not cover, and verify it | exit `77` when a policy matched the scope but not the identity; exit `64` (`no_identity_provided`) when no policy covers the scope at all |

If step 4 produces exit `0` anywhere, the policy is not covering the scope you think it is. Check that the scope prefix matches on `/`-separated segment boundaries, and remember that the *most specific* matching tier wins.

## Coverage {#coverage}

Stated plainly, because parity is easy to imply and expensive to assume.

**Only the generic-OIDC path has live acceptance coverage.** The suite signs and verifies end to end against real Fulcio, Rekor, TesseraCT and dex on every run.

**Every rung here is covered.** The suite proves each rung carries the trust root on its own, air-gapped, with no flag or variable. The rungs are the convention path, an inlined `trusted_root_json`, and a digest-pinned managed-config package delivering it to a machine holding no local copy. The tag-pinned case is covered too, as a refusal.

**The GitHub-issuer and GitLab-issuer paths are documented and reviewed, not test-covered.** The fixture cannot exercise them. dex cannot mint GitHub- or GitLab-shaped claims. Fulcio validates the issuer URL against its own configuration, so there is no way to stand the path up locally.

The configuration above is reviewed against upstream Fulcio's `config/identity/config.yaml`. "Reviewed against upstream" is a weaker statement than "runs in CI", and you should treat it as one.

The practical consequence is [step 4 of the previous section](#verify-setup). Run it against your own stack, on your own issuer, before you rely on it.

## See Also {#see-also}

- [Signing][in-depth-signing]: the sign and verify pipeline, bundle format, and trust-root ladder
- [cosign Parity][in-depth-cosign-parity]: cosign interoperability in both directions
- [`[[trust.policy]]`][config-trust] and [`[trust.sigstore]`][config-trust-sigstore]: the configuration reference for both tables
- [`[managed]`][config-managed]: the fleet configuration tier
- [`ocx package verify`][cmd-package-verify]: flags, exit codes, JSON output
- [CI Integration][guide-ci]: toolchain installs and environment export in a pipeline

[sigstore-public-good]: https://docs.sigstore.dev/about/public-deployment/
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
[tesseract]: https://github.com/transparency-dev/tesseract
[dex]: https://dexidp.io/
[in-depth-signing]: ../explanation/signing.mdx
[in-depth-cosign-parity]: ../explanation/cosign-parity.mdx
[signing-limitations]: ../explanation/signing.mdx#current-limitations
[signing-identity-matching]: ../explanation/signing.mdx#identity-matching
[guide-ci]: ../integration/github-actions.md
[guide-auto-verify]: verify-installs.mdx
[env-external-proxies]: ../integration/proxy-and-ca.md#proxy
[config-trust]: ../reference/configuration.md#keys-trust
[config-trust-sigstore]: ../reference/configuration.md#keys-trust-sigstore
[config-managed]: ../reference/configuration.md#keys-managed
[cmd-package-sign]: ../reference/command-line/package-publishing.md#package-sign
[cmd-package-verify]: ../reference/command-line/package-consuming.md#package-verify
[cmd-config-push]: ../reference/command-line/config.md#config-push
[cmd-config-update]: ../reference/command-line/config.md#config-update
[arg-offline]: ../reference/command-line/index.md#arg-offline
