---
outline: deep
---

# Attestations {#attestations}

A signature answers "who published this." It does not answer "what is inside it." A CI pipeline can sign a binary with the right identity every time and still ship a bundled library with a known CVE — the signature says nothing about composition, because that was never its job.

An attestation closes that gap. It is a structured, signed statement about a package — an SBOM listing every component and version, a provenance record naming the build that produced it, a vulnerability scan result — bound to the exact manifest digest the signature already covers. [`ocx package attest`][cmd-package-attest] publishes one through the same keyless pipeline [signing][in-depth-signing] uses; [`ocx package sbom`][cmd-package-sbom] reads it back, verified.

## Attach an SBOM {#attestations-attach}

The common case is a CycloneDX SBOM generated right after a build. [`ocx package push --sbom <PATH>`][cmd-package-push] attaches it in the same breath as the push — sugar for calling `attest --type cyclonedx` against the digest the push just wrote:

```shell
cyclonedx-cli ... > sbom.json
ocx package push -p linux/amd64 --sbom sbom.json registry.example/pkg:1.0 pkg.tar.gz
```

Attaching a predicate standalone, attaching an SPDX SBOM (`push --sbom` is CycloneDX-only — use `attest --type spdx` or `spdxjson`), attaching more than one predicate type to the same manifest, or attesting something this invocation did not just publish all need [`ocx package attest`][cmd-package-attest] directly:

```shell
ocx package attest -p linux/amd64 --predicate sbom.json --type cyclonedx registry.example/pkg:1.0
```

`--predicate` names the file whose bytes the attestation wraps verbatim; `--type` is one of the cosign-compatible aliases (see [Predicate types](#attestations-types) below) or any absolute predicate-type URI. The command wraps that predicate in a [DSSE][dsse]-enveloped [in-toto][in-toto] Statement naming the target manifest digest as its subject, signs it through the identical keyless pipeline [`sign`][cmd-package-sign] uses — an ephemeral key, a [Fulcio][fulcio] certificate bound to your OIDC identity, a [Rekor][rekor] transparency-log entry — and pushes the bundle as a referrer.

Watch it happen against a real, self-hosted Sigstore stack — attest a CycloneDX SBOM, list it, and extract it back out:

<Terminal src="/casts/user-guide/attestations.cast" title="Attaching a signed SBOM attestation and reading it back" collapsed />

## Read it back {#attestations-read}

[`ocx package sbom`][cmd-package-sbom] is the read-side counterpart to `attest`: it lists every attestation a manifest carries, or extracts one. Every attestation it returns has already gone through the full verification pipeline — referrer discovery, the Fulcio/Rekor/identity chain, the Statement's subject-digest binding. There is no `--no-verify` escape here, for the same reason there is none for [installing a signed package](#attestations-verification): an attestation you have not verified is not an attestation, it is an unverified claim wearing one.

A bare invocation lists what a manifest carries:

```shell
ocx package sbom -p linux/amd64 registry.example/pkg:1.0
```

`--output <PATH|->` extracts one predicate's bytes, byte-exact as the publisher signed them — refusing rather than guessing if more than one attestation matches and `--type` did not narrow it down:

```shell
ocx package sbom -p linux/amd64 --type cyclonedx --output sbom.json registry.example/pkg:1.0
```

For a CycloneDX predicate specifically, `--summary` augments the listing with spec version, component count and the top-level component name, rather than replacing it — a quick read without piping the whole document through `jq`. `--format json` output also gains a `serial_number` field; the plain-text form omits it.

::: info Attestations vs. plain verification
[`ocx package verify --attestation`][cmd-package-verify-attestations] runs the identical cryptographic pipeline and answers only "is there a validly-signed attestation of this type." It never returns the predicate's content — that is what `sbom` is for. Reach for `verify --attestation` in a policy gate that only needs a pass/fail; reach for `sbom` the moment you need the SBOM itself.
:::

## Predicate types {#attestations-types}

`--type` accepts the same short aliases [cosign][cosign] uses, so a predicate produced for one keyless-signing tool reads the same in the other:

| Alias | Resolves to |
|---|---|
| `cyclonedx` | `https://cyclonedx.org/bom` |
| `spdx`, `spdxjson` | the SPDX JSON predicate type |
| `slsaprovenance` | SLSA provenance v0.2 — `attest` refuses to publish this (exit 64, `provenance_version_unsupported`); pass `slsaprovenance1` |
| `slsaprovenance02` | SLSA provenance v0.2, explicit spelling — refused the same way as `slsaprovenance`; pass `slsaprovenance1` |
| `slsaprovenance1` | SLSA provenance v1 |
| `link`, `vuln`, `openvex` | in-toto link, vulnerability-scan, and [OpenVEX][openvex] predicates |
| `custom` | wraps the predicate bytes in cosign's `{Data, Timestamp}` envelope before signing |

Any other value is taken as an absolute predicate-type URI and stored byte-exact — attesting a predicate type OCX has no built-in alias for costs nothing but the full URI. See the [`attest` reference][cmd-package-attest] for the exit codes and the 15 MiB predicate size bound.

## Verification is never optional {#attestations-verification}

The same rule [automatic verification][guide-auto-verify] applies to signatures applies to attestations: `sbom` and `verify --attestation` cannot return an unverified result, because an unverified attestation is not meaningfully different from a text file an attacker wrote. A registry that cannot prove referrer support, a Rekor instance that will not confirm an inclusion proof, or a certificate identity that does not match your policy all fail the read — they never fall back to "here's the content anyway."

That is a deliberate asymmetry with plain file distribution: an SBOM attached this way is only ever useful *because* it carries the same trust chain a signature does. An SBOM you could not verify would tell you nothing you did not already have to take on faith.

Verification proves an attestation is authentic and unmodified as of when it was signed, not that it is current — a newer SBOM or provenance record can exist for the same package, and there is no rollback protection against an older, still-validly-signed attestation being served instead.

::: tip Learn more
[`attest` reference][cmd-package-attest] — full options, exit codes, and the `--type` alias table.
[`sbom` reference][cmd-package-sbom] — listing, extraction, `--summary`, and the JSON envelope.
[`package verify` reference][cmd-package-verify-attestations] — the `--attestation`/`--type` verification-only mode.
[Signing][in-depth-signing] — the keyless pipeline `attest` shares with `sign`, trust root mechanics, and offline verification.
:::

<!-- external -->
[dsse]: https://github.com/secure-systems-lab/dsse
[in-toto]: https://github.com/in-toto/attestation
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
[cosign]: https://github.com/sigstore/cosign
[openvex]: https://github.com/openvex/spec

<!-- commands -->
[cmd-package-attest]: ../reference/command-line.md#package-attest
[cmd-package-sbom]: ../reference/command-line.md#package-sbom
[cmd-package-sign]: ../reference/command-line.md#package-sign
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-package-verify-attestations]: ../reference/command-line.md#package-verify-attestations

<!-- internal -->
[in-depth-signing]: ../in-depth/signing.md
[guide-auto-verify]: ../user-guide.md#supply-chain-auto-verify
