---
outline: deep
---
# Self-hosted Sigstore

You run your own registry because the artifacts are internal. Signing them against [public Sigstore][sigstore-public-good] puts the *identity* of every signer, and the time of every signature, into a world-readable transparency log — even though the artifact itself never leaves your network. Running your own [Fulcio][fulcio] and [Rekor][rekor] keeps both inside.

That is the easy half. The hard half is the trust root: a self-hosted CA appears in no TUF root, so every machine that verifies has to be told what to trust. Doing that by hand — copy a file, set an environment variable, per host — is the step that makes internal signing die of paperwork.

This page is the fleet answer. What you have to run, where the trusted root comes from, how it reaches every machine without a per-host step, and which OIDC issuer to point Fulcio at — including GitHub Actions, which is where most teams actually sign from.

The mechanics of signing and verifying themselves are in [Signing][in-depth-signing]; this page assumes them.

## What you have to run {#components}

Four components. Only the first two are OCX's concern; the other two are what those two depend on.

| Component | What it does | Do you have a choice? |
|---|---|---|
| [Fulcio][fulcio] | Exchanges an OIDC token for a short-lived X.509 certificate whose SAN *is* the caller's identity. This is the CA. | No — OCX speaks Fulcio |
| [Rekor][rekor] | Append-only transparency log. Records a signature or an attestation and returns a Signed Entry Timestamp that proves *when* it was logged — `hashedrekord` entries for signatures, `dsse` entries for attestations. | No — v1 only, see [Current limitations][signing-limitations] |
| A certificate-transparency log | Fulcio embeds a Signed Certificate Timestamp in every certificate it issues, and the verifier checks it. No CT log means no verifiable certificates. | [TesseraCT][tesseract] or a Trillian-backed CTFE |
| An OIDC issuer | Mints the token Fulcio trusts. **This is the decision that determines whether the stack can run air-gapped** — see [the issuer matrix](#issuers). | Yes, and it matters |

This repository runs exactly that as its acceptance fixture: seven services in `test/docker-compose.yml` under the `sigstore` profile ([dex][dex], Fulcio, Rekor, [TesseraCT][tesseract], two Trillian services, MySQL). `test/sigstore/README.md` documents the wiring, and it is a working reference for a small internal deployment.

```sh
cd test && docker compose --profile sigstore up -d
```

## Where the trusted root comes from {#trusted-root}

A *trusted root* is one JSON document carrying three things that must travel together:

- the **Fulcio CA certificate(s)** — the anchor the leaf certificate chains to;
- the **certificate-transparency log public keys** — without them the embedded SCT cannot be checked, and OCX refuses trust material that lacks them with exit `78`;
- the **Rekor public key** — pinned, so the Signed Entry Timestamp verifies with no call to Rekor. This is what makes [`--offline`][arg-offline] verification possible.

A bare CA certificate is *not* a trust root. It is the most natural thing to reach for and it verifies nothing.

Produce it once, from the stack you just stood up:

```sh
cosign trusted-root create --certificate-chain fulcio-ca.crt.pem --out trusted_root.json
```

```sh
python3 test/sigstore/generate-trusted-root.py    # what this repository uses for its fixture
```

Regenerate it when the CA rotates or a CT log is added. It is public material — there is no secret in it — so it can be committed, published to a registry, or served over plain HTTP inside your network without care.

## Getting it onto every machine {#distribution}

OCX resolves the trust root through [six rungs](./signing.md#trust-root), first hit wins. Four of them are yours to choose between. They are listed here cheapest-first in operator effort, which is the inverse of how most people discover them:

### 1. Ship one `config.toml` to the fleet {#distribution-managed}

The one that scales. `[trust.sigstore]` is read from the operator `config.toml` tiers, and the [`[managed]`][config-managed] tier makes one published `config.toml` the fleet's configuration. Put the trusted root in the payload:

```toml
# the config.toml you publish
[trust.sigstore]
trusted_root = "trusted_root.json"   # path, relative to THIS file
```

```sh
ocx config push -i registry.corp.example/ops/ocx-config:1 ./fleet-config.toml
```

[`config push`][cmd-config-push] takes one config file, so `trusted_root.json` has to sit beside it — that relative path resolves against the declaring file's own directory. At publish time `config push` reads it and inlines it as `trusted_root_json`, so what the fleet receives is self-contained: it names no path on anyone's disk. Consumers adopt the seed once:

```sh
ocx self setup --managed-config registry.corp.example/ops/ocx-config@sha256:<digest>
```

::: warning Seed the managed tier by digest
A payload carrying `trusted_root_json` is **ignored** unless the `[managed]` seed is digest-pinned. Otherwise the trust root would arrive over the very channel it exists to verify; the circularity has to be broken by a pinned seed, not by policy. Path-form `trusted_root` arriving from the managed tier is likewise ignored, with a warning — a fleet payload cannot name a path on someone else's machine.
:::

Rotation from there is [`ocx config update`][cmd-config-update] against a new digest, or a cascade tag if you seeded by tag *and* keep the trust root out of the payload.

### 2. Write it into `/etc/ocx/config.toml` {#distribution-system}

If you already own the machine image or run a configuration-management tool, the system-scope config is the shortest path — and it is the **strongest** one: a `[trust.sigstore]` declared at system scope is locked, and no user, home, or managed tier can replace it.

```toml
# /etc/ocx/config.toml
[trust.sigstore]
trusted_root = "sigstore/trusted-root.json"   # relative to /etc/ocx/
```

A relative path anchors to the declaring file's own directory, so the same line means the same file regardless of anyone's working directory.

### 3. Drop the file at the convention path {#distribution-convention}

No configuration at all — put the document where OCX already looks:

```sh
install -Dm644 trusted_root.json "$OCX_HOME/sigstore/trusted-root.json"
```

Absent, it falls through to the next rung. Present but unreadable, it fails — that is deliberate: a trust root you meant to install and cannot read is not the same thing as one you never installed.

### 4. `--sigstore-trusted-root` / `OCX_SIGSTORE_TRUSTED_ROOT` {#distribution-flag}

The flag and its environment variable are for one-off overrides — a debugging session, a CI job pinning a specific root, a machine that is not part of the fleet. Reaching for them as the *deployment* mechanism is how you end up setting an environment variable in every shell profile on every host.

## The issuer matrix {#issuers}

Fulcio does not mint identities; it validates tokens from an issuer you name in its own `config.yaml` and turns them into certificates. Which issuer you pick decides how much of the stack can live inside your network.

| Setup | Air-gappable? | What it needs |
|---|---|---|
| GitHub.com Actions + GitHub-hosted runner | **No** | The runner cannot reach an internal registry at all — this is a network problem, not a Sigstore one |
| GitHub.com Actions + self-hosted runner | Nearly | One egress allowlist to `token.actions.githubusercontent.com`, so Fulcio can fetch the issuer's JWKS |
| GitHub Enterprise Server + self-hosted runner | **Yes** | Fully internal |
| GitLab self-managed | **Yes** | Fully internal |
| Generic OIDC ([dex][dex], Keycloak) + any runner | **Yes** | Fully internal; the only path with live acceptance coverage here — see [Coverage](#coverage) |

The egress in row 2 is Fulcio's, not the runner's: Fulcio validates the token's signature against the issuer's published keys, so it must reach the issuer's JWKS endpoint. Nothing else in the pipeline leaves the network.

### Fulcio configuration per issuer {#issuers-fulcio}

Each issuer is one entry in Fulcio's `config.yaml`. The `Type` selects how Fulcio derives the certificate SAN from the token's claims, and the values below are the ones upstream Fulcio defines in `config/identity/config.yaml`:

```yaml
# GitHub.com Actions
oidc-issuers:
  https://token.actions.githubusercontent.com:
    issuer-url: https://token.actions.githubusercontent.com
    client-id: sigstore
    type: github-workflow
```

```yaml
# GitHub Enterprise Server — read the exact issuer URL off a real token (below)
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

Fulcio ships further types — `email`, `kubernetes`, `circleci-job`, `buildkite-job`, `codefresh-workflow`, `buddy-pipeline`, `chainguard-identity` — each deriving the SAN from that platform's claims. Consult upstream's `config/identity/config.yaml` for the current set rather than this page: it is Fulcio's list, not OCX's.

### Read the identity, do not guess it {#issuers-read}

Every worked example below is the *expected shape*. The strings that matter are whatever your issuer actually put in the token and Fulcio actually put in the certificate — and they differ by platform version and by how a job was triggered. Sign once and read them off the output:

```sh
ocx package sign -p linux/amd64 \
  --fulcio-url https://fulcio.corp.example \
  --rekor-url https://rekor.corp.example \
  registry.corp.example/acme/mytool:1.0.0
```

`Certificate identity` and `Certificate OIDC issuer` in that output are, byte for byte, what a policy has to match. Copy them; do not retype them.

### Worked policies {#issuers-policies}

**GitHub Actions** (either github.com or GHES — only the host differs). The identity is the workflow file at a ref, not a person:

```toml
# $OCX_HOME/config.toml — or the config.toml you publish to the fleet
[[trust.policy]]
scope = "registry.corp.example/acme"

[trust.policy.keyless]
identity    = "https://github.com/acme/mytool/.github/workflows/release.yml@refs/tags/v1.0.0"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

Pinning an exact tag ref means every release needs a policy edit. The usual shape is a regexp over the ref, with the workflow path fixed:

```toml
[[trust.policy]]
scope = "registry.corp.example/acme"

[trust.policy.keyless]
identity_regexp = "https://github\\.com/acme/mytool/\\.github/workflows/release\\.yml@refs/tags/v.*"
oidc_issuer     = "https://token.actions.githubusercontent.com"
```

**GitLab self-managed.** The identity is the CI config file at a ref; note GitLab's doubled slash between project path and config path:

```toml
[[trust.policy]]
scope = "registry.corp.example/acme"

[trust.policy.keyless]
identity_regexp = "https://gitlab\\.corp\\.example/acme/mytool//\\.gitlab-ci\\.yml@refs/tags/v.*"
oidc_issuer     = "https://gitlab.corp.example"
```

**Generic OIDC.** A human or a service account, identified by email:

```toml
[[trust.policy]]
scope = "registry.corp.example/acme"

[trust.policy.keyless]
identity    = "release-bot@corp.example"
oidc_issuer = "https://sso.corp.example"
```

`oidc_issuer` is always compared byte-for-byte — it is never dialled, so it does not have to be an address the verifying machine can reach. `identity_regexp` is wrapped as `\A(?:…)\z` before it is compiled, so it must match the **entire** SAN and you never write the anchors yourself — a pattern that looks like a substring match is a full match.

## `identity_regexp` is an authorization boundary {#authorization}

A regexp that matches more than you meant is not a slightly loose config. It is a grant.

```toml
# Every project on the instance. Anyone who can create one can sign your packages.
identity_regexp = "https://gitlab\\.corp\\.example/.*"
```

Under that policy, an engineer who creates `gitlab.corp.example/anyone/scratch` and pushes a `.gitlab-ci.yml` gets a certificate that satisfies it. The signature is genuine, Rekor logged it, and OCX accepts it — because you said that identity was trusted.

Three rules that keep the boundary real:

1. **Anchor everything you can.** Fix the project path and the workflow or CI-config path; leave the wildcard on the ref alone. `…/acme/mytool//\.gitlab-ci\.yml@refs/tags/v.*` is a grant to one file in one project. `…/acme/.*` is a grant to a namespace, which is only as tight as who can create projects in it.
2. **Escape the dots.** `gitlab.corp.example` as a regexp matches `gitlabXcorpYexample`. Every literal `.` in a host or a filename is `\.`.
3. **The CI side is half the control.** A policy pinning `release.yml@refs/tags/v*` is only as strong as who can push a tag and who can edit that file. Pair it with protected tags (or protected branches), CODEOWNERS on the workflow or CI-config file, and a limit on who can create projects in the namespace the scope covers. OCX enforces *which identity signed*; your forge enforces *who can become that identity*.

For scopes where the answer must not be negotiable, declare the policy at system scope (`/etc/ocx/config.toml`). A system-scope policy is admission-authoritative: no user, project, or managed-tier policy can outbid it with a narrower scope or join its accepted set. The full resolution model is in [`[[trust.policy]]`][config-trust].

## Verifying the setup end to end {#verify-setup}

Four steps, and the fourth is the one that matters — a check you have never seen fail is not a check.

**1. Sign.**

```sh
ocx package sign -p linux/amd64 \
  --fulcio-url https://fulcio.corp.example \
  --rekor-url https://rekor.corp.example \
  registry.corp.example/acme/mytool:1.0.0
```

**2. Verify on a machine that has only the fleet configuration** — no flag, no environment variable:

```sh
ocx package verify -p linux/amd64 registry.corp.example/acme/mytool:1.0.0
```

Exit `0` with no `--sigstore-trusted-root` and no `--certificate-identity` proves both halves at once: the trust root arrived through configuration, and a `[[trust.policy]]` matched.

**3. Verify offline**, to prove the pinned Rekor key is really in the trust material:

```sh
ocx package verify --offline -p linux/amd64 registry.corp.example/acme/mytool:1.0.0
```

**4. Break it on purpose.** Each of these must fail, with the exit code named:

| Break | Expected |
|---|---|
| Edit `identity` in the policy to a wrong-but-plausible value | exit `77` — identity mismatch |
| Point `oidc_issuer` at a different issuer | exit `77` — issuer mismatch |
| Remove the trust root from every rung and run `--offline` | exit `78` — no pinned Rekor key |
| Strip the `ctlogs` entries out of the trusted root | exit `78` — `trust root carries no CT log key` |
| Sign from a project the policy does not cover, and verify it | exit `77` when a policy matched the scope but not the identity; exit `64` (`no_identity_provided`) when no policy covers the scope at all |

If step 4 produces exit `0` anywhere, the policy is not covering the scope you think it is — check the scope prefix matches on `/`-separated segment boundaries, and remember that the *most specific* matching tier wins.

## Coverage {#coverage}

Stated plainly, because parity is easy to imply and expensive to assume:

**The generic-OIDC path is the only one with live acceptance coverage.** The suite signs and verifies end to end against real Fulcio, Rekor, TesseraCT and dex on every run.

**Every distribution rung on this page is covered.** The suite proves each one carries the trust root on its own, air-gapped, with no flag and no environment variable: the convention path, an inlined `trusted_root_json`, and a digest-pinned managed-config package delivering it to a machine holding no local copy. The tag-pinned case is covered too — as a refusal.

**The GitHub-issuer and GitLab-issuer paths are documented and reviewed, not test-covered.** They cannot be exercised by the fixture: dex cannot mint GitHub- or GitLab-shaped claims, and Fulcio validates the issuer URL against its own configuration, so there is no way to stand the path up locally. The configuration above is reviewed against upstream Fulcio's `config/identity/config.yaml` — but "reviewed against upstream" is a weaker statement than "runs in CI", and you should treat it as one.

The practical consequence is [step 4 of the previous section](#verify-setup). Run it against your own stack, on your own issuer, before you rely on it.

## See Also {#see-also}

- [Signing][in-depth-signing] — the sign/verify pipeline, bundle format, cosign interoperability, offline verification
- [`[[trust.policy]]`][config-trust] and [`[trust.sigstore]`][config-trust-sigstore] — the configuration reference for both tables
- [`[managed]`][config-managed] — the fleet configuration tier
- [`ocx package verify`][cmd-package-verify] — flags, exit codes, JSON output
- [CI Integration][guide-ci] — toolchain installs and environment export in a pipeline

[sigstore-public-good]: https://docs.sigstore.dev/about/public-deployment/
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
[tesseract]: https://github.com/transparency-dev/tesseract
[dex]: https://dexidp.io/
[in-depth-signing]: ./signing.md
[signing-limitations]: ./signing.md#current-limitations
[guide-ci]: ./ci.md
[config-trust]: ../reference/configuration.md#keys-trust
[config-trust-sigstore]: ../reference/configuration.md#keys-trust-sigstore
[config-managed]: ../reference/configuration.md#keys-managed
[cmd-package-verify]: ../reference/command-line.md#package-verify
[cmd-config-push]: ../reference/command-line.md#config-push
[cmd-config-update]: ../reference/command-line.md#config-update
[arg-offline]: ../reference/command-line.md#arg-offline
