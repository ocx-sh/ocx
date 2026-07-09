---
outline: deep
---
# Signing

You want to know whether the binary you are about to run came from the person or pipeline you trust — not just that the download arrived intact.

Checksums answer "did the file change in transit?" They do not answer "who built this?" A checksum tells you the bytes match a known digest; it cannot tell you whether an attacker replaced both the binary and the checksum file on a compromised mirror.

OCX solves this by attaching a [Sigstore][sigstore] keyless signature to each package manifest at publish time. The signature binds a cryptographic identity — a GitHub Actions workflow URL or an email address — to the exact manifest digest. At verify time, OCX checks that the identity matches what you specified and that the cryptographic proof is valid. There is no key management: the signing key is ephemeral and the certificate is issued by [Fulcio][fulcio], with an audit trail in [Rekor][rekor].

The user-facing surface — sign a release, verify what you install — lives in the [Supply-Chain Integrity section of the user guide][user-supply-chain].

## Trust Root {#trust-root}

OCX verifies [Fulcio][fulcio] certificates against a trust root: a set of DER-encoded X.509 CA certificates. You supply one in two ways, in precedence order:

- **`--trust-root <PATH>`** on `ocx package verify`, or the [`OCX_SIGSTORE_TRUST_ROOT`][env-sigstore-trust-root] environment variable (the flag wins) — a PEM file of one or more `CERTIFICATE` blocks, loaded by `TrustRoot::load_from_pem`. This is the seam the acceptance suite uses to inject the `fake_fulcio` self-signed root so the verify pipeline trusts test-minted certificates.
- **The embedded production root** — `TrustRoot::load_embedded` is intended to ship a bundled [TUF][sigstore-tuf] trust root compiled into the binary. It is **stubbed** in this release: with no flag or env override, verify exits 78 (`TrustRootUnavailable`).

So today, `ocx package verify` requires an explicit `--trust-root` / `OCX_SIGSTORE_TRUST_ROOT`. The chain check and every downstream verification step run fully once a root is supplied.

## Referrers Capability Cache {#referrers-cache}

[OCI Referrers][oci-referrers-spec] discovery requires the registry to implement `GET /v2/{repo}/referrers/{digest}`. OCX probes once per registry and caches the result so repeated sign or verify calls pay no extra round-trip.

Cache location: `$OCX_HOME/state/referrers/{registry_slug}.json`

The `{registry_slug}` is the registry hostname with any character outside `[a-zA-Z0-9._-]` replaced by an underscore (`_`). For example, `ghcr.io` becomes `ghcr_io`.

Each cache file is a JSON object with four fields:

| Field | Type | Description |
|-------|------|-------------|
| `registry` | string | Registry hostname |
| `supported` | `"Supported"` \| `"Unsupported"` | Result of the last probe |
| `probed_at` | UNIX timestamp | Wall-clock time of the probe (UTC) |
| `ttl_seconds` | integer | Seconds after `probed_at` the entry remains valid |

The cache is advisory and fail-open: a missing or corrupt file triggers a fresh probe; the probe result then overwrites the file atomically (temp-file rename, mode `0600` on Unix). Entries are valid for **6 hours** (`TTL_SECS = 6 * 3600`); after that, the next sign or verify invocation re-probes automatically. Pass `--no-cache` to bypass the cache for a single invocation.

## OCI 1.1 Referrers Hard-Fail Policy {#referrers-hard-fail}

OCX does not implement a fallback to the [cosign][cosign] tag scheme (`sha256-<digest>.sig`). If a registry returns a non-referrers error response (anything other than HTTP 200 or an explicit "unsupported referrers" status), the sign and verify operations exit 84 (`ReferrersUnsupported`).

This is an explicit design choice: a silent fallback would let signatures be published to a registry that cannot guarantee their discoverability, or let a verification path succeed against a stale or unreachable fallback tag. Hard-fail makes the dependency on OCI 1.1 explicit so operators know exactly which registries are compatible.

:::info Which registries support OCI 1.1 Referrers?

OCX `package sign` / `package verify` require OCI Distribution Spec v1.1 Referrers API. As of May 2026:

- **Supported:** [Zot][zot], [Harbor][harbor] 2.9+, JFrog Artifactory 7.90+ (including `ocx.sh`), Amazon ECR, Azure ACR, Google Artifact Registry, Red Hat Quay 3.12+.
- **Not supported (exit 84):** CNCF Distribution `registry:2` / `registry:3` (no Referrers API — it serves only the tag-schema fallback, which OCX does not use), [GHCR][ghcr] (GitHub Container Registry), [Docker Hub][docker-hub]. Use a registry from the supported list for signed packages.

This is by design — OCX never writes legacy `sha256-<digest>.sig` fallback tags (ADR S1-F). The hard error gives operators a clear "change registry" signal rather than silent downgrade.
:::

## Sigstore Bundle Format and Storage {#bundle-storage}

A signature is a [Sigstore bundle v0.3][sigstore-bundle] — a JSON envelope carrying:

- The [Fulcio][fulcio]-issued short-lived certificate (chain from leaf to CA root)
- The ECDSA P-256 signature over the subject manifest's SHA-256 digest
- The [Rekor][rekor] Signed Entry Timestamp (SET) for the log entry

OCX pushes the bundle as an OCI referrer of the subject manifest. The referrer artifact's media type is `application/vnd.dev.sigstore.bundle.v0.3+json`. The raw blob lands in `$OCX_HOME/blobs/` alongside other OCI blobs, identified by its own SHA-256 digest and referenced in the subject manifest's referrers index.

The blob is not referenced by any candidate or current symlink — it is found via the [OCI Referrers API][oci-referrers-spec] at verify time, not via the install symlink tree.

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

## Slice Boundary {#slice-boundary}

**This release** wires the complete keyless pipeline: OIDC token acquisition, ephemeral ECDSA P-256 keypair generation, the [Fulcio][fulcio] certificate request, the [Rekor][rekor] log entry, [Sigstore bundle v0.3][sigstore-bundle] assembly, the referrer push, and the full five-check verify path — certificate chain against the trust root, Rekor SET, signature over the subject digest, identity match, issuer match. Sign and verify run end-to-end; their exit-code and flag contracts are stable.

What is **not** yet done is production hardening against public-good Sigstore. The pipeline is exercised only against the in-repo fake Sigstore stack — see [Current Limitations](#current-limitations). The Fulcio and Rekor clients are hand-rolled against that fake stack's wire shapes, the embedded TUF trust root is stubbed, and the Rekor SET is checked over a fake-stack payload format. Wiring and testing against public Fulcio/Rekor/TUF is tracked as a follow-up.

**Rekor v2 transition:** Bundles signed against a Rekor v2 instance carry RFC 3161 TSA timestamps instead of SETs. OCX treats these as exit 83 (`RekorUnavailable`) with `VerifyErrorKind::RekorSetAbsentTsaPresent`. Full TSA verification ships in a future slice when [sigstore-rs][sigstore-rs] lands a Rekor v2 client.

**DSSE / `ocx package attest`:** DSSE attestation signing and verification are not implemented. The verify path rejects a DSSE-envelope bundle with `NoUsableBundle` (exit 79). Deferred until [sigstore-rs][sigstore-rs] ships DSSE support.

## Current Limitations {#current-limitations}

The pipeline is verified end-to-end against the in-repo fake Sigstore stack, not the public-good Fulcio/Rekor/TUF. Until production hardening lands, be aware:

- **Single-hop certificate chain.** The leaf is verified directly against a trust-root CA; intermediate certificates in the bundle are not walked. A real Fulcio leaf signed by an intermediate will not validate unless that intermediate is itself in the supplied trust root.
- **No certificate temporal-validity check.** The leaf's `notBefore` / `notAfter` are not checked against the [Rekor][rekor] integrated time, so a certificate that had expired by verify time is not yet rejected on that basis.
- **Rekor SET format is fake-stack-specific.** The Signed Entry Timestamp is Ed25519-verified over OCX's own deterministic payload, not the public Rekor canonical wire format. Verification against public-good Rekor is not yet supported.
- **No Merkle inclusion proof.** Only the Rekor SET (inclusion promise) is checked; the transparency-log inclusion and consistency proofs are not verified.
- **Rekor key is fetched, not pinned.** The Rekor public key is fetched from `--rekor-url/api/v1/log/publicKey` at verify time (trust-on-first-use) rather than pinned in the trust root.
- **Embedded TUF trust root is stubbed.** `TrustRoot::load_embedded` returns `TrustRootUnavailable`; you must pass `--trust-root` / [`OCX_SIGSTORE_TRUST_ROOT`][env-sigstore-trust-root].

Do not treat a green `ocx package verify` against production Sigstore as a completed cryptographic verification until these are addressed.

:::warning Offline verification (Slice 1)
`ocx package verify` requires a live network connection in Slice 1. Offline cache-hit verification is planned for Slice 2. If you pass `--offline`, the command exits 81 (`PolicyBlocked`).

`ocx package sign` rejects `--offline` with exit 77 (`PermissionDenied`) — the policy is on the action (sign cannot proceed offline by design), distinct from verify's read-side block.
:::

:::tip Custom Sigstore endpoints
`--fulcio-url` and `--rekor-url` point the CLI at a private or self-hosted Sigstore deployment instead of the public Fulcio/Rekor. `validate_sigstore_url` accepts `http://` only for loopback hosts (`127.0.0.0/8`, `::1`, `localhost`); any non-loopback target must be `https://`, so the SSRF guard stays active. The clients are hand-rolled against the fake stack's wire shapes today (see [Current Limitations](#current-limitations)).
:::

## Signing Flow Summary {#signing-flow}

1. OCX resolves the OIDC identity token using the [token precedence order][cmd-package-sign-token-precedence]:
   `--identity-token-file` → `--identity-token-stdin` → [`OCX_IDENTITY_TOKEN`][env-identity-token]
   → ambient CI detection → interactive browser OAuth.
2. An ephemeral ECDSA P-256 keypair is generated in memory.
3. The ephemeral public key is sent to [Fulcio][fulcio] with the OIDC token; Fulcio issues a short-lived certificate binding the key to the OIDC identity.
4. The subject manifest's SHA-256 digest is signed with the ephemeral private key. The key is zeroized immediately after signing.
5. The log entry is posted to [Rekor][rekor]; the response contains the SET.
6. The certificate, signature, and SET are assembled into a [Sigstore bundle v0.3][sigstore-bundle] and pushed to the registry as a referrer of the subject manifest.

## See Also {#see-also}

- [`package sign` reference][cmd-package-sign] — flags, token-source precedence, exit codes, CI example
- [`package verify` reference][cmd-package-verify] — flags, identity matching options, exit codes
- [Configuration reference → `[[trust.policy]]`][config-trust] — schema, scope matching, most-specific-wins resolution, operator-vs-project tier precedence
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

<!-- commands -->
[cmd-package-sign]: ../reference/command-line.md#package-sign
[cmd-package-sign-token-precedence]: ../reference/command-line.md#package-sign
[cmd-package-verify]: ../reference/command-line.md#package-verify

<!-- reference -->
[config-trust]: ../reference/configuration.md#keys-trust

<!-- environment -->
[env-identity-token]: ../reference/environment.md#ocx-identity-token
[env-sigstore-trust-root]: ../reference/environment.md#ocx-sigstore-trust-root

<!-- user guide -->
[user-supply-chain]: ../user-guide.md#supply-chain
