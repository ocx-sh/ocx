---
outline: deep
---
# Signing

You want to know whether the binary you are about to run came from the person or pipeline you trust — not just that the download arrived intact.

Checksums answer "did the file change in transit?" They do not answer "who built this?" A checksum tells you the bytes match a known digest; it cannot tell you whether an attacker replaced both the binary and the checksum file on a compromised mirror.

OCX solves this by attaching a [Sigstore][sigstore] keyless signature to each package manifest at publish time. The signature binds a cryptographic identity — a GitHub Actions workflow URL or an email address — to the exact manifest digest. At verify time, OCX checks that the identity matches what you specified and that the cryptographic proof is valid. There is no key management: the signing key is ephemeral and the certificate is issued by [Fulcio][fulcio], with an audit trail in [Rekor][rekor].

The user-facing surface — sign a release, verify what you install — lives in the [Supply-Chain Integrity section of the user guide][user-supply-chain].

## Trust Root {#trust-root}

OCX verifies [Fulcio][fulcio] certificates against a trust root: a set of DER-encoded X.509 CA certificates. Two construction paths exist in the code:

- **`TrustRoot::load_embedded()`** — intended to ship a bundled [TUF][sigstore-tuf] trust root asset compiled into the binary. In Slice 1 this path returns `TrustRootUnavailable` (exit 78); the production trust bundle ships in Slice 2.
- **`TrustRoot::load_from_pem(pem_bytes)`** — loads one or more `CERTIFICATE` PEM blocks from a supplied byte slice. Used by the acceptance test stack to inject the `fake_fulcio` self-signed root, so the verify pipeline trusts test-minted certificates without shipping production roots.

Until `load_embedded` is wired in Slice 2, `ocx package verify` against a real [Sigstore][sigstore] deployment will exit 78. The trust-root loading path and all downstream verification logic is otherwise fully functional once a root is supplied.

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

- **Supported:** `registry:2` v2.8.0+, [Harbor][harbor] 2.5+, [Zot][zot], JFrog Artifactory (recent), `ocx.sh`.
- **Not supported (exit 84):** [GHCR][ghcr] (GitHub Container Registry), [Docker Hub][docker-hub]. No public roadmap to add Referrers API as of 2026-05. Use a v1.1-compatible registry for signed packages.

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

At verify time, `--certificate-identity` is checked against the SAN and `--certificate-oidc-issuer` is checked against the issuer extension. Both checks are exact-match in Slice 1. Wildcard and regex matching are planned for Slice 2 — the flags are intentionally named for the eventual match-policy expansion.

A concrete GitHub Actions identity looks like this:

```
--certificate-identity https://github.com/<org>/<repo>/.github/workflows/<file>.yml@refs/heads/main
--certificate-oidc-issuer https://token.actions.githubusercontent.com
```

The `@refs/heads/main` suffix is the ref the workflow ran on; pin to the exact ref you publish from. The `<file>.yml` is the path inside `.github/workflows/` of the workflow file that signed.

## Slice Boundary {#slice-boundary}

OCX ships signing and verification in two slices:

**Slice 1 (this release):** Wires flag parsing, OIDC token acquisition, referrers-capability probing, trust-root loader infrastructure (`load_from_pem` fully functional, `load_embedded` returns `TrustRootUnavailable`), and the full offline-mode rejection. Both stub boundaries surface a typed error that classifies to exit 78 (`ConfigError`): `ocx package sign` returns `SignErrorKind::PipelinePending` (the `pipeline_pending` detail discriminant) before the [sigstore-rs][sigstore-rs] pipeline would run, and `ocx package verify` returns `VerifyErrorKind::TrustRootUnavailable`. Both are operator-visible contracts — CI scripts may condition on exit 78 (and the snake_case `detail` discriminant) to detect the slice boundary, with no panic output to scrape. Exit codes and flag contracts are stable; scripts may condition on them today.

**Slice 2 (planned):** Ships the complete sigstore-rs integration: Fulcio CSR construction, ECDSA P-256 keypair generation, Rekor log entry, and the full five-check verify path (certificate chain against TUF root, Rekor SET, signature over subject digest, identity match, issuer match). Also ships the production TUF trust root bundle (`load_embedded`) and cache-hit verify in `--offline` mode.

**Rekor v2 transition:** Bundles signed against a Rekor v2 instance carry RFC 3161 TSA timestamps instead of SETs. OCX v1 treats these as exit 83 (`RekorUnavailable`) with `VerifyErrorKind::RekorSetAbsentTsaPresent`. Full TSA verification ships in a future slice when [sigstore-rs][sigstore-rs] lands a Rekor v2 client.

**DSSE / `ocx package attest`:** DSSE attestation signing is not in Slice 1 or Slice 2. Deferred until [sigstore-rs][sigstore-rs] ships DSSE signing support (no upstream PR as of 2026-05). DSSE verification (reading external attestations) is on the Slice 2 roadmap.

:::warning Offline verification (Slice 1)
`ocx package verify` requires a live network connection in Slice 1. Offline cache-hit verification is planned for Slice 2. If you pass `--offline`, the command exits 81 (`PolicyBlocked`).

`ocx package sign` rejects `--offline` with exit 77 (`PermissionDenied`) — the policy is on the action (sign cannot proceed offline by design), distinct from verify's read-side block.
:::

:::tip Custom Sigstore endpoints
`--fulcio-url` and `--rekor-url` point the CLI at a private or self-hosted Sigstore deployment instead of the public Fulcio/Rekor. `validate_sigstore_url` accepts `http://` only for loopback hosts (`127.0.0.0/8`, `::1`, `localhost`); any non-loopback target must be `https://`, so the SSRF guard stays active. The end-to-end signing pipeline lands in Slice 2 (see [Slice Boundary](#slice-boundary)).
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

<!-- environment -->
[env-identity-token]: ../reference/environment.md#ocx-identity-token

<!-- user guide -->
[user-supply-chain]: ../user-guide.md#supply-chain
