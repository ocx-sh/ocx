---
outline: deep
---
# cosign Parity

You already sign container images with [cosign][cosign], and you are being asked to adopt a second tool. The question that decides it is not whether OCX signs — it is whether the signature OCX writes is one your existing `cosign verify` step accepts, and whether the signature your existing pipeline writes is one OCX accepts back.

Both hold. OCX and cosign read and write the same [Sigstore][sigstore] documents: a [bundle v0.3][sigstore-bundle] published as an [OCI referrer][oci-referrers-spec], and the older `sha256-<hex>.sig` and `sha256-<hex>.att` sidecar tags. Neither tool needs a flag to read what the other wrote, and a signature does not have to be re-issued to cross between them.

This page states exactly how far that goes — which commands verify which artifacts, where the two tools deliberately behave differently, and what evidence backs each claim. Where OCX diverges it is a design decision with a reason, recorded here rather than smoothed over. For how signing works at all, see [Signing][in-depth-signing]; this page assumes it.

Signing a package with OCX and verifying it with upstream cosign, then signing a second package with cosign and verifying it with OCX — one registry, one [Fulcio][fulcio], one [Rekor][rekor]:

<Terminal src="/casts/in-depth/cosign-parity.cast" title="Signing with one tool and verifying with the other, in both directions" collapsed />

## What Interoperates {#interop-matrix}

Interop is bounded by four independent choices: which tool signs, which wire shape carries the signature, whether the signer is keyless or a key pair, and whether the registry implements the [OCI 1.1 Referrers API][oci-referrers-spec] or only the fallback tag.

Every combination of those four works, in both directions. The tables below are not a summary of intent — each row is a test that publishes a real package, signs it, verifies it with the other tool, then corrupts one byte of the signature and requires the refusal. See [What Proves This](#evidence).

### OCX signs, cosign verifies {#ocx-to-cosign}

| Wire shape | Signer | Registry | `cosign verify` |
|---|---|---|---|
| Bundle referrer | Keyless | Referrers API | Accepts, transparency-log check included |
| Bundle referrer | Keyless | Fallback tag | Accepts, transparency-log check included |
| Bundle referrer | `--key` | Referrers API | Accepts with `--key` and `--insecure-ignore-tlog` |
| Bundle referrer | `--key` | Fallback tag | Accepts with `--key` and `--insecure-ignore-tlog` |
| `sha256-<hex>.sig` sidecar | Keyless | Referrers API | Accepts, transparency-log check included |
| `sha256-<hex>.sig` sidecar | Keyless | Fallback tag | Accepts, transparency-log check included |
| `sha256-<hex>.sig` sidecar | `--key` | Referrers API | Accepts with `--key` and `--insecure-ignore-tlog` |
| `sha256-<hex>.sig` sidecar | `--key` | Fallback tag | Accepts with `--key` and `--insecure-ignore-tlog` |

The default `bundle` format needs no opt-in on either side: cosign discovers an OCX bundle through the Referrers API where the registry has one, and through the `sha256-<hex>` fallback index where it does not. `--signature-format simplesigning` exists to write the older sidecar as well, not to make discovery work.

A key-mode signature carries no transparency-log entry unless one was requested ([the Rekor rule](#divergences)), so cosign needs `--insecure-ignore-tlog` to accept one — the same flag it needs for its own key-mode signatures. Under keyless, no such flag is involved in either direction.

### cosign signs, OCX verifies {#cosign-to-ocx}

| Wire shape | Signer | Registry | `ocx package verify` |
|---|---|---|---|
| Bundle referrer | Keyless | Referrers API | Accepts, `discovery_method` `referrers_api` |
| Bundle referrer | Keyless | Fallback tag | Accepts, `discovery_method` `fallback_tag` |
| Bundle referrer | `--key` | Referrers API | Accepts with `--key`, `discovery_method` `referrers_api` |
| Bundle referrer | `--key` | Fallback tag | Accepts with `--key`, `discovery_method` `fallback_tag` |
| `sha256-<hex>.sig` sidecar | Keyless | Referrers API | Accepts with [`--allow-unlogged-signature`](#sidecar-tlog) |
| `sha256-<hex>.sig` sidecar | Keyless | Fallback tag | Accepts with [`--allow-unlogged-signature`](#sidecar-tlog) |
| `sha256-<hex>.sig` sidecar | `--key` | Referrers API | Accepts with `--key`, `discovery_method` `sidecar_tag` |
| `sha256-<hex>.sig` sidecar | `--key` | Fallback tag | Accepts with `--key`, `discovery_method` `sidecar_tag` |

Verify reads all three discovery paths with no flag, and reports which one carried the signature it accepted in the `signatures[]` array of `ocx --format json package verify`.

The two keyless sidecar rows are the one place a flag is unavoidable, and it is not an OCX quirk: `cosign attach signature` writes no transparency-log annotation, so **cosign refuses those artifacts too**, needing `--insecure-ignore-tlog` for exactly the same reason. The two tools agree on the artifact and on the opt-out; see [The Keyless Sidecar Rule](#sidecar-tlog) for what the opt-out costs.

### Attestations {#attestations}

[DSSE][dsse]-enveloped [in-toto][in-toto] attestations interoperate as bundle referrers, both directions, on registries with and without the Referrers API.

| Direction | Command | Result |
|---|---|---|
| OCX attests | `cosign verify-attestation` | Accepts; `--check-claims` validates the subject binding |
| cosign attests | `ocx package verify --attestation --type <TYPE>` | Accepts; predicate type resolved and matched |

Bundle-referrer interop is tested keyless only. Key-mode attestation uses the same signing path as key-mode signatures, which is covered above, but no test drives a key-mode attestation across the two tools through the bundle shape.

`ocx package verify --attestation` also reads the legacy `sha256-<hex>.att` sidecar tag that `cosign attach attestation` writes. That shape is reachable **by tag and by nothing else**: its manifest declares neither `artifactType` nor `subject`, and cosign publishes no attestation artifact type at all, so no Referrers listing can reach it. OCX looks for it only under `--attestation`, and only once nothing bundle-shaped verified, so an ordinary run never pays the extra request.

The two `.att` key models rest on different evidence, and the difference is worth knowing. The **key-mode** shape is pinned to real `cosign attach attestation` output, captured as a committed fixture. The **keyless** shape is not producible by cosign v3.1.1 at all — `attach attestation` takes no `--certificate`, and `attest` no longer writes the tag — so it is covered by unit tests that repackage cosign's own certificate, envelope and log entry into the measured layer shape. Nothing in the suite verifies a keyless `.att` that cosign itself emitted, because cosign cannot emit one.

A `--type` that does not match what the artifact carries is a not-found condition on both sides rather than a verification failure: OCX exits 79 (`attestation_not_found`), cosign reports that none of the attestations matched the requested predicate type.

### Attached SBOMs {#attached-sboms}

`cosign attach sbom` writes the SBOM document itself to a `sha256-<hex>.sbom` tag — a third tag-only shape, and the one that is not a signature. `ocx package sbom` reads it.

| Direction | Command | Result |
|---|---|---|
| cosign attaches | `ocx package sbom --no-verify` | Lists the document `verified: false`, labelled by the layer's own media type |
| cosign attaches | `ocx package sbom --verify` | Refuses: `unsigned_rejected_by_policy`, exit 77 |

The two rows are one contract, not a limitation. **`cosign attach sbom` signs nothing** — it prints "Attaching SBOMs this way does not sign them" — and no cosign command signs that tag afterwards, so an attached SBOM has no signer for a policy to check. Under `--no-verify` OCX lists it exactly as it lists any other unsigned attachment; under `--verify` it refuses it exactly as it refuses an unsigned referrer. There is no third mode in which the shape verifies. A signed SBOM is an *attestation* — `cosign attest --predicate sbom.json`, which is the bundle-referrer shape in the table above.

Discovery is by tag and by nothing else, for the same structural reason `.att` is: the manifest declares neither `artifactType` nor `subject`, so no Referrers listing reaches it. What differs from `.att` is the layer — an `.sbom` layer keeps the SBOM document's own media type rather than carrying a signature — which is why the reader is a document reader on the listing path rather than a third signature reader.

The layer type OCX reads is cosign's, which is not always the registered spelling. `--type spdx` — cosign's **default** — writes `text/spdx+json`, where OCX's own attach path writes `application/spdx+json`; `--type cyclonedx --input-format xml` writes `application/vnd.cyclonedx+xml`. All of those list. `--type syft` writes `application/vnd.syft+json`, which is refused by name (`sbom_media_type_unsupported`, exit 65): no in-toto predicate type names syft's native format, so there is nothing to label it with.

There is no reverse direction. OCX has no writer for an unsigned SBOM sidecar and does not intend one — `ocx package push --sbom` and `ocx package attest` produce a signed attestation instead.

Also read: cosign's OCI 1.1 SBOM **referrer**, which `COSIGN_EXPERIMENTAL=1 cosign attach sbom --registry-referrers-mode oci-1-1` writes with `artifactType: application/vnd.dev.cosign.artifact.sbom.v1+json` while typing its layer by the document. Both shapes land in the same listing.

## Addressing a Package {#addressing}

An OCX package is a multi-platform [OCI index][oci-image-index], so a signature can sit on the index or on one platform manifest beneath it. `-p` is what decides which, and it also decides the reference cosign has to be given.

`ocx package sign` with no `-p` signs **the index itself** — the same shape cosign uses for a multi-platform tag. Nothing special is needed on the cosign side: the tag resolves to the index, which is where the signature is.

```sh
cosign verify \
  --certificate-identity ci@example.com \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  registry.example/acme/tool:1.0.0
```

`ocx package sign -p linux/amd64` narrows the subject to that platform's manifest instead. `ocx package verify -p linux/amd64` resolves it back the same way, but cosign has no notion of a package and resolves whatever reference you hand it — so the tag still resolves to the index, where this signature is *not*, and cosign reports a discovery failure that reads like an interop problem. Give it the platform manifest's own digest:

```sh
# --resolve is required: it walks index -> manifest and reports the manifest.
digest=$(ocx --format json package inspect --resolve -p linux/amd64 \
  registry.example/acme/tool:1.0.0 | jq -r '.packages[0].pinned_digest')

cosign verify \
  --certificate-identity ci@example.com \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  "registry.example/acme/tool@${digest}"
```

:::warning `--resolve` is not optional here
Without it, `pinned_digest` reports the **index** digest — the reference that carries no per-platform signature. The flag is what makes `inspect` select a platform and walk down to its manifest, and it is the difference between a `cosign verify` that works and one that reports nothing found.
:::

## Where OCX Differs {#divergences}

Two behaviours differ from cosign's defaults. Both are deliberate, and neither changes the wire format — an artifact produced under either still verifies with the other tool.

| Behaviour | cosign | OCX | Why |
|---|---|---|---|
| Rekor upload under a key | On by default | Off by default; `--rekor-upload` opts in | `rekor_url` defaults to the *public* log. An on-by-default key path would publish a private artifact's digest and signer identity to a world-readable log the first time it ran — the opposite of what choosing a key over keyless usually means |
| Rekor upload under keyless | Opt-out available | Mandatory; `--no-rekor-upload` is a usage error (exit 64) | A Fulcio certificate lives about ten minutes. The log timestamp is the only lasting proof the signature happened while it was valid, so a keyless signature without one is unverifiable later by construction |

Two things that look like divergences are not. Both tools omit the DSSE `keyid` member — cosign matches candidate signatures on it, so writing one makes `cosign verify --key` accept none of them; OCX names the key in `verificationMaterial.publicKey.hint` where cosign does. And on a registry without the Referrers API, both tools write the `sha256-<digest>` fallback index rather than refusing; see [Publishing a Referrer][in-depth-signing-registries].

On the read side, `keyid` is a lookup hint and never a security decision — OCX accepts an envelope whose `keyid` is absent, empty, or hostile, and the value decides nothing.

:::warning Exit 84 is a write-path code
A registry with neither the Referrers API nor a fallback tag makes `ocx package verify` exit 79 (no signatures found), never 84. Verify reads both shapes, so "nothing is there" is the verdict rather than a capability refusal. 84 belongs to the commands that must *write* a referrer, and only when the fallback write is refused too.
:::

## The Keyless Sidecar Rule {#sidecar-tlog}

A `sha256-<hex>.sig` or `sha256-<hex>.att` sidecar carries its verification material in layer annotations. For a keyless signature the `dev.sigstore.cosign/bundle` annotation — the offline Rekor entry — is required, and a sidecar without it is refused with exit 65 (`signature_invalid`). **Both shapes are held to one gate**: the `.att` reader was built against the `.sig` path rather than reimplemented beside it, so there is no shape that verifies on weaker evidence than the other.

The reason is the certificate's lifetime. [Fulcio][fulcio] issues a certificate valid for roughly ten minutes, and the transparency-log entry's `integratedTime` is the only thing that places the signature inside that window. Without an entry there is no instant to check the certificate against, so a certificate that expired months ago stays as acceptable as one issued a moment ago.

`--allow-unlogged-signature` accepts the sidecar anyway. It exists for air-gapped CI, where the entry could not be fetched or was never written.

**What it stops checking.** Three checks do not run — they are skipped, not satisfied by a substitute:

- The Signed Entry Timestamp is not verified against the log's public key.
- The logged body is not bound to the artifact. That check requires the logged entry to name this payload's digest and carry this signature — a `hashedrekord` body for a `.sig`, a `dsse` body for an `.att`. Without it, a real entry for a *different* artifact would be indistinguishable from an entry about the bytes in hand.
- The certificate validity window is not evaluated at all. There is no trustworthy instant to evaluate it at, and taking the instant from the certificate's own `notBefore` would be circular — a check that asks the certificate when it was valid and then judges the certificate against that answer can never fail.

**What still runs.** The certificate chain to the trust root, the SCT, the signature over the subject digest, and identity matching against `--certificate-identity` / `--certificate-oidc-issuer` or a matching [`[[trust.policy]]`][config-trust]. On an `.att`, the statement's subject binding and any `builder` pin the policy carries run too. A tampered signature is still refused under the flag.

The practical consequence: under `--allow-unlogged-signature` a signature is proven to come from a certificate the trust root vouches for, and not proven to have been made while that certificate was valid. An accepted signature reports neither `signed_at` nor `rekor_log_index`, because there is no timestamp to report — the absence of those fields is how a downstream consumer tells the two cases apart.

The flag is inert everywhere else. It does not affect the bundle path, where transparency evidence stays mandatory under keyless, nor key-mode verification of either sidecar shape, which never reads the annotation at all — a key signature's trust rests on the committed public key rather than on a signing instant.

:::info cosign's counterpart
`--insecure-ignore-tlog` is the same idea on the cosign side, and cosign needs it for artifacts `cosign attach signature` produced, because that command writes no annotation either. The difference is scope: cosign's flag applies to any verification, while OCX's applies only to the keyless sidecar shapes and is ignored on every other path.
:::

## Known Gaps {#gaps}

Five things do not work, or do not work fully. They are gaps rather than divergences — nothing here is a decision OCX would defend, and each is a place where prose could easily overclaim.

- **cosign's fallback-tag write drops annotations.** On a registry without the Referrers API, cosign's own fallback index loses `dev.sigstore.bundle.content`, `dev.sigstore.bundle.predicateType` and `org.opencontainers.image.created` from the referrer descriptor ([sigstore/cosign#4641][gh-cosign-4641]). `artifactType` survives. OCX reads these artifacts regardless, and OCX's own fallback write preserves all four — the reader is not weakened to match.
- **Signing is sha256-only.** [`ocx package sign`][cmd-package-sign] and [`ocx package attest`][cmd-package-attest] refuse a subject addressed by sha384 or sha512 with exit 65 (`subject_digest_unsupported`), before anything is published or logged. cosign is sha256-only in the same places — the in-toto Statement binds on `sha256`, and the sidecar tag truncates the digest to 64 characters — so the alternative is a signature that publishes and then cannot be verified. Verification is unaffected: an already-published artifact still reads back whatever it was written under.
- **`ocx package copy` does not carry sidecar-tag signatures.** [`copy --referrers`][cmd-package-copy] follows referrer chains, and a `sha256-<hex>.sig` / `.att` sidecar is an ordinary tag rather than a referrer — so a simplesigning signature does not survive a mirror copy. The referrer-attached bundle shape does. Re-sign at the destination when the mirrored artifact needs a sidecar.
- **KMS key backends are not implemented.** `awskms://`, `gcpkms://`, `azurekms://`, `hashivault://` and `k8s://` are recognised by name and refused with exit 85 (`unsupported_key_backend`) — from `--key`, from a `key = "…"` signer in a matched [`[[trust.policy]]`][config-trust], and from a managed-config payload alike. cosign implements all five. The distinct code exists so a script can tell "not built yet" from a malformed config (78) or a missing file (74) without parsing stderr.
- **Rekor v1 only.** OCX targets Rekor v1 entries; a bundle from a Rekor v2 instance is rejected with exit 83. This bounds interop with any cosign configured against a v2 log. See [Current Limitations][in-depth-signing-limitations].

## What Proves This {#evidence}

Every accept and refuse on this page is a test in the acceptance suite, run against upstream cosign — the real binary from `ghcr.io/sigstore/cosign/cosign:v3.1.1`, pinned to an exact version so a passing run is attributable to one — driving a live [Fulcio][fulcio], [Rekor][rekor] and registry.

The suite covers **16 image-level cells** (the two eight-row tables in [What Interoperates](#interop-matrix)) and **7 attestation and attached-SBOM cells**. Twelve of the image cells carry one artifact through all six of the steps below; the four key-mode cosign-signs cells assert the refusal half against a second, identically-constructed artifact in a shared parametrized test.

The attached-SBOM cell ([Attached SBOMs](#attached-sboms)) runs a shorter sequence, because nothing in it is signed and so nothing can be corrupted. It substitutes a **control**: the same command on the same subject exits 79 *before* cosign attaches anything, so the listing that follows is attributable to the attachment rather than to a reader that reports something for every subject.

<Steps>

1. Publish a package to a registry of the cell's kind — [Zot][zot] for the Referrers API, CNCF Distribution for the fallback tag.
2. Sign it with the cell's tool, wire shape and key model.
3. Assert exactly one signature is discoverable, so the cell cannot pass on a second artifact left behind by an earlier step.
4. Assert the other tool **accepts** it.
5. Corrupt one byte of the signature, and assert the corruption reached the registry.
6. Assert the other tool **refuses** it, with an exact exit code and an exact message.

</Steps>

Step 6 is what makes the green meaningful. A test that only asserts acceptance passes just as well against a verifier that accepts everything; pinning the refusal's exit code and message means the check has a reachable red state, and the corruption in step 5 is verified to have landed rather than assumed.

**Claims on this page that no test backs**, stated rather than dropped:

- Key-mode attestation across the two tools. The [attestation table](#attestations) is keyless only.
- **Index-level signing verified by tag**, the first recipe in [Addressing a Package](#addressing). That `ocx package sign` without `-p` signs the index is read from the source; every cell in the matrix signs one platform manifest with `-p` and verifies against its digest, so the tag-reference path is not measured.
- `cosign attach sbom` in either direction ([Known Gaps](#gaps)).
- **cosign's own keyless Rekor opt-out**, asserted in the [divergence table](#divergences) from cosign's documentation. Nothing here drives `cosign sign --tlog-upload=false` under keyless.
- **`--allow-unlogged-signature` being inert elsewhere.** That it cannot lift a bundle-path or key-mode refusal is read from the source — the flag reaches only the sidecar verifier — not asserted by a test.
- **Rekor v2 rejection with exit 83.** The mapping is in the source; no Rekor v2 instance is exercised.
- **The keyless `.att` refusal and its opt-out.** Both are unit-tested against a fixture that repackages cosign's own certificate, envelope and log entry into the measured `.att` layer shape — not against an artifact cosign emitted, because cosign v3.1.1 cannot emit a keyless `.att` ([Attestations](#attestations)). The key-mode `.att` *is* pinned to real `cosign attach attestation` output.
- **The `--resolve` recipe in [Addressing a Package](#addressing).** Both halves are tested separately — that `--resolve` yields a per-platform digest, and that cosign verifies a manifest digest — but no test pipes one into the other.
- The registry support lists in [Signing][in-depth-signing-registries] are drawn from vendor documentation, not exercised here — the suite runs against Zot and CNCF Distribution only, one registry per capability.

## See Also {#see-also}

- [Signing][in-depth-signing] — how signing works: trust roots, bundle storage, identity matching, keyless versus a key
- [`package sign` reference][cmd-package-sign] — every flag, including `--signature-format` and `--rekor-upload`
- [`package verify` reference][cmd-package-verify] — every flag, the `signatures[]` report shape, and every exit code
- [Self-hosted Sigstore][in-depth-self-hosted-sigstore] — running your own Fulcio and Rekor, as the cast above does
- [Configuration reference → `[[trust.policy]]`][config-trust] — pinning an identity once instead of passing it per command

<!-- external -->
[sigstore]: https://www.sigstore.dev/
[cosign]: https://github.com/sigstore/cosign
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
[sigstore-bundle]: https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_bundle.proto
[oci-referrers-spec]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#listing-referrers
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[dsse]: https://github.com/secure-systems-lab/dsse
[in-toto]: https://github.com/in-toto/attestation
[zot]: https://zotregistry.dev/

<!-- commands -->
[cmd-package-sign]: ../reference/command-line.md#package-sign
[cmd-package-attest]: ../reference/command-line.md#package-attest
[cmd-package-verify]: ../reference/command-line.md#package-verify
[cmd-package-sbom]: ../reference/command-line.md#package-sbom
[cmd-package-copy]: ../reference/command-line.md#package-copy

<!-- reference -->
[config-trust]: ../reference/configuration.md#keys-trust

<!-- internal -->
[in-depth-signing]: ./signing.md
[in-depth-signing-limitations]: ./signing.md#current-limitations
[in-depth-signing-registries]: ./signing.md#referrers-write
[in-depth-self-hosted-sigstore]: ./self-hosted-sigstore.md

<!-- issues -->
[gh-cosign-4641]: https://github.com/sigstore/cosign/issues/4641
