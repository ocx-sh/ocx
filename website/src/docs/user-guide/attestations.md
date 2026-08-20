---
outline: deep
---

# Attestations {#attestations}

A signature answers "who published this." It does not answer "what is inside it." A CI pipeline can sign a binary with the right identity every time and still ship a bundled library with a known CVE — the signature says nothing about composition, because that was never its job.

An attestation closes that gap. It is a structured statement about a package — an SBOM listing every component and version, a provenance record naming the build that produced it, a vulnerability scan result — bound to the exact manifest digest the signature already covers. [`ocx package attest`][cmd-package-attest] publishes one through the same keyless pipeline [signing][in-depth-signing] uses; [`ocx package push --sbom`][cmd-package-push] is sugar for the CycloneDX case; [`ocx package sbom`][cmd-package-sbom] reads either kind back.

## Attach an SBOM {#attestations-attach}

The common case is a CycloneDX SBOM generated right after a build. But not every place a build happens carries a signing identity — a laptop, a local script, a CI job that has not been wired for OIDC — and the SBOM still needs to travel with the package for whatever reads it later: a dependency scanner, an auditor, the next stage of the same pipeline. Requiring a stronger floor for that metadata than for the executable artifact sitting beside it would be backwards.

So `attach` has two shapes, and OCX picks between them by what it can see in the environment, not by a flag you have to remember to pass:

::: info Two shapes, one command
`ocx package attest` and `ocx package push --sbom` **sign** when a signing identity is visible — an override token, or a detected CI platform like GitHub Actions — and **attach the document raw** when nothing is visible. The rule mirrors `ocx package push`: either there is a credential to act on, or there is not, and the command tells you honestly which happened rather than degrading silently.
:::

### Signed attach {#attestations-attach-signed}

With an identity available, [`ocx package push --sbom <PATH>`][cmd-package-push] attaches the SBOM in the same breath as the push — sugar for calling `attest --type cyclonedx` against the digest the push just wrote:

```shell
cyclonedx-cli ... > sbom.json
ocx package push -p linux/amd64 --sbom sbom.json registry.example/pkg:1.0 pkg.tar.gz
```

Attaching a predicate standalone, attaching an SPDX SBOM (`push --sbom` is CycloneDX-only — use `attest --type spdx` or `spdxjson`), attaching more than one predicate type to the same manifest, or attesting something this invocation did not just publish all need [`ocx package attest`][cmd-package-attest] directly:

```shell
ocx package attest -p linux/amd64 --predicate sbom.json --type cyclonedx registry.example/pkg:1.0
```

`--predicate` names the file whose bytes the attestation wraps verbatim; `--type` is one of the cosign-compatible aliases (see [Predicate types](#attestations-types) below) or any absolute predicate-type URI. With a signing identity present, the command wraps that predicate in a [DSSE][dsse]-enveloped [in-toto][in-toto] Statement naming the target manifest digest as its subject, signs it through the identical keyless pipeline [`sign`][cmd-package-sign] uses — an ephemeral key, a [Fulcio][fulcio] certificate bound to your OIDC identity, a [Rekor][rekor] transparency-log entry — and pushes the bundle as a referrer.

Watch it happen against a real, self-hosted Sigstore stack — sign a CycloneDX SBOM, list it, and extract it back out:

<Terminal src="/casts/user-guide/attestations.cast" title="Attaching a signed SBOM attestation and reading it back" collapsed />

### Unsigned attach {#attestations-attach-unsigned}

With no signing identity visible, `attest` and `push --sbom` still succeed — they publish the SBOM document itself as the referrer's payload, typed by its own media type (`application/vnd.cyclonedx+json` for CycloneDX, `application/spdx+json` or `text/spdx` for SPDX), with no [DSSE][dsse] envelope, no Fulcio certificate, and no Rekor entry. This is the same wire shape [`cosign attach sbom`][cosign-attach-sbom] and [`oras attach`][oras-attach] write — an ocx-attached unsigned SBOM reads back with those tools, and one they attached reads back with `ocx package sbom`.

An unsigned SBOM carries exactly the trust of the unsigned package it describes: same repository, same set of people who can push to it, same registry access controls. It is not a weaker copy of the signed case — it is a different claim. A signed attestation says "this identity vouches for this document"; an unsigned attach says "this document was published alongside this package," which is already true of the package's binary layers, and now true of its SBOM too.

The check is exactly "is there a signing identity visible" — never a network call, and never the interactive browser sign-in, which is a *prompt for* an identity rather than *evidence of* one. If a signing identity *is* visible but the acquisition then fails — Fulcio unreachable, an ambient CI token rejected — that is a hard error, not a fallback to the unsigned form: a CI job configured to sign that silently published unsigned would look identical, on the wire, to a job that never intended to sign, hiding the exact misconfiguration an operator needs to see.

One predicate type is refused in unsigned mode outright: SLSA provenance. Without a DSSE envelope there is no builder identity attached to the statement, so an unsigned provenance record carries no attribution at all — `attest --type slsaprovenance1` with no signing identity visible exits 64 (`unsigned_type_unsupported`) before any network call, rather than publishing a document that looks like provenance and proves nothing.

The same cast, run again with no signing identity present, attaches a second SBOM unsigned — then reads both documents back with `ocx package sbom --no-verify`, the reading mode covered next in [Read it back](#attestations-read):

<Terminal src="/casts/user-guide/attestations-unsigned.cast" title="Attaching an unsigned SBOM and reading it back permissively" collapsed />

## Read it back {#attestations-read}

Attaching and reading are two separate questions, decided independently: attaching asks whether a signing identity is *visible* — visible, it signs; absent, it attaches raw (see [Attach an SBOM](#attestations-attach)). Reading asks whether verification is being *demanded* — demanded, it runs the full pipeline and refuses anything unsigned; not demanded, it runs no cryptography at all and reads whatever is there. [`ocx package sbom`][cmd-package-sbom] is where that second question gets answered.

The problem the second question exists to solve: a consumer auditing a dependency has usually configured nothing — no `[[trust.policy]]`, no certificate identity to check against. Refusing to hand them an SBOM until they set up Sigstore turns a read into a wall. But a consumer who *did* configure a policy, or who typed `--certificate-identity` expecting it to be checked, must never be handed an unsigned document dressed up as though it passed — that would make a policy decorative rather than enforced.

`ocx package sbom` resolves one of two modes per invocation, and states which one it used:

- **Demand** — every listed document carries a signature that passed the full pipeline: referrer discovery, the Fulcio/Rekor/identity chain, the Statement's subject-digest binding. An unsigned attach is **refused**, never listed — the policy names who must have signed, and a raw attachment has no signer at all.
- **Permissive** — nothing is checked and no cryptography runs. A signed bundle's [DSSE][dsse] payload is extracted exactly as a raw attachment's bytes are read, and both are reported `verified: false` with no signer identity, because none was checked. This is what makes the command usable with no Sigstore setup at all: the consumer above gets the SBOM either way.

Which mode runs is resolved from what you asked for and what your environment already resolves to — never a silent default with no way to see which one you got:

| You pass | A `[[trust.policy]]` covers the package? | Mode |
|---|---|---|
| nothing | no | Permissive |
| nothing | yes | Demand |
| `--certificate-identity` + `--certificate-oidc-issuer` | *(overrides policy either way)* | Demand |
| `--verify` | no, and no identity flags either | **Error**, exit 64 |
| `--verify` | yes, or identity flags given | Demand |
| `--no-verify` | *(any)* | Permissive |

`--verify` and `--no-verify` last-win when both are typed, the same as every `--x`/`--no-x` pair in ocx — combining them is not an error. `--no-verify` **does** conflict with the certificate flags: supplying an identity while refusing to check it is contradictory, not overridden, so that combination is a usage error regardless of order. `--verify` with nothing to verify against is also a usage error (`no_identity_provided`, exit 64) rather than a silent fall-back to permissive — an operator who typed `--verify` must not have it quietly ignored.

::: info Reading the reader's own polarity
Attach and read mirror each other on purpose: **attach** signs when a signing identity is *visible*; **read** demands verification when an identity *source* — flags or policy — is visible. Neither side ever downgrades silently when a check that should run then fails; both fail loudly instead.
:::

A verified entry carries `verified: true` plus the certificate identity, issuer and signed-at timestamp that vouch for it; an unverified entry carries `verified: false` with those three fields **omitted**, never emitted empty — an empty identity would read as a verification that failed to render, not as "there was nothing to verify." The listing's own `summary.verification` field (`verified` or `unverified`) names which mode produced the whole run, so a script never has to infer it from the rows: under Demand mode an unverified row cannot occur at all, and under Permissive mode nothing else can.

`--output <PATH|->` extracts one predicate's bytes, byte-exact as the publisher wrote them, from whichever candidate the mode's own trust class resolved to:

```shell
ocx package sbom -p linux/amd64 --type cyclonedx --output sbom.json registry.example/pkg:1.0
```

More than one matching candidate refuses rather than guessing — naming every colliding referrer digest and every distinct predicate type in the set, so `--type` has a value to narrow with. Under Permissive mode, writing a document nothing vouched for prints one line to stderr saying so (`SBOM is unverified: no signature over referrer <digest> was checked, so nothing vouches for what it says`); the bytes written to `PATH` (or stdout) are unaffected, so a script piping `--output -` into a file still gets exactly the predicate's bytes with the warning visible separately.

For a CycloneDX predicate specifically, `--summary` augments the listing with spec version, component count and the top-level component name, rather than replacing it — a quick read without piping the whole document through `jq`. `--format json` output also gains a `serial_number` field; the plain-text form omits it. `--summary` reads a document exactly the same way regardless of mode: whether anyone vouches for a component list says nothing about whether the list parses.

::: info Attestations vs. plain verification
[`ocx package verify --attestation`][cmd-package-verify-attestations] runs the identical cryptographic pipeline Demand mode does, and answers only "is there a validly-signed attestation of this type" — it never returns the predicate's content, and it never considers an unsigned referrer a candidate at all, in either mode. Reach for `verify --attestation` in a policy gate that only needs a pass/fail on signed attestations; reach for `sbom` the moment you need the SBOM itself, whichever way it was attached.
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

::: warning Unsigned attach is SBOM-only
The raw, unsigned wire shape exists for the three SBOM media types only — CycloneDX and both SPDX serializations. Every other type in the table — provenance, `link`, `vuln`, `openvex`, `custom` — has no signing identity to fall back from: with none visible, `attest` refuses those types outright (exit 64, `unsigned_type_unsupported`) rather than publish an untyped or attribution-free document. Signing them normally is unaffected.
:::

## Verification and trust floors {#attestations-verification}

Demand mode keeps the rule [automatic verification][guide-auto-verify] states for signatures: it cannot return an unverified result for a signed candidate, because an unverified attestation is not meaningfully different from a text file an attacker wrote. A registry that cannot prove referrer support, a Rekor instance that will not confirm an inclusion proof, or a certificate identity that does not match your policy all fail the read for that candidate — they never fall back to "here's the content anyway." An unsigned attach under Demand mode gets the same treatment: refused, not silently listed.

Permissive mode is not a hole in that rule — it is a floor the reader chooses, deliberately, the moment no identity source is on the table (or explicitly, via `--no-verify`). Nothing here is ever picked *for* an operator who demanded verification: the mode table above has exactly one row that resolves to Permissive with an identity source present, and it requires typing `--no-verify` by hand. Every document Permissive mode returns is labeled `verified: false`, every time, with no path that upgrades it into looking checked.

Verification proves an attestation is authentic and unmodified as of when it was signed, not that it is current — a newer SBOM or provenance record can exist for the same package, and there is no rollback protection against an older, still-validly-signed attestation being served instead. That caveat applies inside Demand mode only; Permissive mode proves nothing about authenticity either way, so freshness was never on the table there to begin with.

::: tip Learn more
[`attest` reference][cmd-package-attest] — full options, exit codes, and the `--type` alias table.
[`sbom` reference][cmd-package-sbom] — listing, extraction, `--summary`, and the JSON envelope for both modes.
[`package verify` reference][cmd-package-verify-attestations] — the `--attestation`/`--type` verification-only mode.
[Signing][in-depth-signing] — the keyless pipeline `attest` shares with `sign`, trust root mechanics, and offline verification.
:::

<!-- external -->
[dsse]: https://github.com/secure-systems-lab/dsse
[in-toto]: https://github.com/in-toto/attestation
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
[cosign]: https://github.com/sigstore/cosign
[cosign-attach-sbom]: https://github.com/sigstore/cosign/blob/main/doc/cosign_attach_sbom.md
[oras-attach]: https://oras.land/docs/commands/oras_attach/
[openvex]: https://openvex.dev/spec

<!-- commands -->
[cmd-package-attest]: ../reference/command-line.md#package-attest
[cmd-package-sbom]: ../reference/command-line.md#package-sbom
[cmd-package-sign]: ../reference/command-line.md#package-sign
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-package-verify-attestations]: ../reference/command-line.md#package-verify-attestations

<!-- internal -->
[in-depth-signing]: ../in-depth/signing.md
[guide-auto-verify]: ../user-guide.md#supply-chain-auto-verify
