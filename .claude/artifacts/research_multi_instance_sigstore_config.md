# Research: multi-instance Sigstore configuration

## Metadata

**Date:** 2026-11-25
**Domain:** security
**Triggered by:** `.agents/discussions/signing-dogfood-mirror.md`, the configuration grammar that replaces the single global `[trust.sigstore]` table (named or address-keyed instance tables bound by reference, a pool of trust roots tried in turn, instance material inline in each identity entry, or one global instance)
**Expires:** 2027-05-25

Returned by one researcher (sonnet). The prompt named the four shapes and asked for evidence on each, with no ranking. Four of the five documentation-only fetches came back thin; that is said where it matters. The researcher read `website/src/content/docs/explanation/signing.mdx` § Trust Root only and did not read `.claude/artifacts/adr_offline_verify_trust_cache.md`.

## Direct Answer

All four shapes are in use. The sigstore policy-controller names an instance as a resource and binds each authority to it by reference. `containers/image` writes the material inline under a registry scope. The GitHub CLI holds several roots and selects one by the issuer organization of the signature's leaf certificate. sigstore-go pools the certificate authorities of one trusted root and takes the first that verifies. cosign and sigstore-python take one instance per run. Sigstore's own unit for "one instance" is the `ClientTrustConfig` document, a `TrustedRoot` plus a `SigningConfig`. No guidance and no incident was found on pooling several Fulcio roots against binding an instance to a scope.

## Findings

### How each tool describes, holds and selects an instance

- **sigstore-go, certificate verification: pooled.** `verifyLeafCertificate` loops over all `FulcioCertificateAuthorities()`, returns on the first that verifies and discards earlier errors; nothing there ties a CA to an identity or issuer. <https://raw.githubusercontent.com/sigstore/sigstore-go/main/pkg/verify/certificate.go> `TrustedMaterialCollection` is a slice of `TrustedMaterial`; the docs do not say whether it tries each member. `SelectService` picks the first service with the highest supported API version valid now. <https://pkg.go.dev/github.com/sigstore/sigstore-go/pkg/root>
- **gh attestation: bound by the certificate's issuer.** `--custom-trusted-root` reads a JSONL file, one trusted root per line. Each line is grouped by the lowest certificate issuer organization of its Fulcio CA (`sigstore.dev`, `GitHub, Inc.`, or a custom value). At verify time `chooseVerifier` reads the issuer organization of the bundle's leaf certificate and selects one verifier; with no match it errors and does not try each root. <https://github.com/cli/cli/blob/trunk/pkg/cmd/attestation/verification/sigstore.go> Public repositories use the public-good instance, private ones GitHub's own. <https://docs.github.com/en/enterprise-cloud@latest/actions/concepts/security/artifact-attestations>
- **policy-controller: named resources, selected by reference.** A `TrustRoot` resource is defined by `remote` (TUF mirror plus root), `repository` (serialized TUF repository) or `sigstoreKeys` (inline CAs, CT logs, tlogs, TSAs). A `ClusterImagePolicy` authority names one with `trustRootRef` under `keyless`, `ctlog` or `rfc3161timestamp`. Images match policies by `spec.images` glob; all matching policies must pass; authorities inside one policy are OR'd. The docs do not describe an OR across authorities with different `trustRootRef` values. <https://docs.sigstore.dev/policy-controller/overview/>
- **containers/image `policy.json`: inline, bound by registry scope.** A `sigstoreSigned` requirement takes exactly one of `keyPath`, `keyData`, `fulcio` or `pki`; a `fulcio` object carries `caPath` or `caData` plus `oidcIssuer` and `subjectEmail`, and the requirement needs a Rekor public key. Requirements sit under transport, then scope (registry, wildcard host, repository, tag); only the most specific matching scope applies. <https://github.com/containers/image/blob/main/docs/containers-policy.json.5.md>
- **Kyverno `verifyImages`: inline per attestor entry.** Each entry carries its own `rekor` and `ctlog` blocks and `keyless` identity fields; no per-entry Fulcio roots field is documented, custom deployments use global `--tufRoot` and `--tufMirror`. <https://release-1-18-0.kyverno.io/docs/policy-types/cluster-policy/verify-images/sigstore/>
- **cosign: one instance per invocation.** `--trusted-root` and `--signing-config` each take one file; `cosign initialize --mirror --root` sets one TUF source. `cosign trusted-root create` takes repeatable `--fulcio`, `--rekor`, `--ctfe` and `--tsa`, so one file can hold several CAs; the docs are silent on how cosign picks among them. <https://raw.githubusercontent.com/sigstore/cosign/main/doc/cosign_trusted-root_create.md>, <https://raw.githubusercontent.com/sigstore/cosign/main/doc/cosign_initialize.md>
- **sigstore-python: one per run.** `--trust-config` since 3.0.0; expects a v0.2 `SigningConfig` since 4.0.0. <https://github.com/sigstore/sigstore-python/blob/main/CHANGELOG.md>

### The three documents

All defined in <https://raw.githubusercontent.com/sigstore/protobuf-specs/main/protos/sigstore_trustroot.proto>.

- `TrustedRoot`, media type `application/vnd.dev.sigstore.trustedroot.v0.2+json`: `tlogs`, `certificate_authorities`, `ctlogs`, `timestamp_authorities`. Each tlog entry has `base_url`, hash algorithm, public key, `log_id`, `checkpoint_key_id` and operator.
- `SigningConfig`, `application/vnd.dev.sigstore.signingconfig.v0.2+json`: signing endpoints, `rekor_tlog_urls` with `rekor_tlog_config` and `tsa_urls` with `tsa_config`.
- `ClientTrustConfig`, `application/vnd.dev.sigstore.clienttrustconfig.v0.1+json`: "One particular instance of Sigstore"; `trusted_root` and `signing_config` are both required. It has no list of instances and no registry or identity scope field.
- Rekor v2: `rekor_tlog_urls` defines major API version 2 as Rekor v2. In v2 `log_id` and `checkpoint_key_id` are the same value, and the proto recommends `checkpoint_key_id` because `log_id` may not be unique across deployments.
- Which clients require which document is not stated; the client specification's section on `ClientTrustConfig` and multiple CAs is an empty placeholder. <https://raw.githubusercontent.com/sigstore/architecture-docs/main/client-spec.md>

### Configuration changes and old configuration

- cosign: the 2.6.x flags `--new-bundle-format`, `--trusted-root` and `--use-signing-config` became defaults in v3; the old behaviour stays reachable by disabling them; cosign v4 is to remove roughly half the flags. Whether old flags error, warn or are ignored is not stated. <https://blog.sigstore.dev/cosign-3-0-available/> (2025-10-08) `SIGSTORE_ROOT_FILE`, `SIGSTORE_REKOR_PUBLIC_KEY` and `SIGSTORE_CT_LOG_PUBLIC_KEY_FILE` are a "last resort". <https://docs.sigstore.dev/cosign/system_config/custom_components/>
- sigstore-python 3.0.0 removed `--rekor-url` and `--fulcio-url` and pointed to `--trust-config`; the changelog does not give the error or warning text.

### Security guidance and incidents

- No Sigstore document was found on accepting several Fulcio roots in one pool against binding an instance to a scope, and no incident of identity confusion across instances.
- The nearest primary evidence is that the issuer URL is part of the certificate identity. <https://github.com/sigstore/fulcio/issues/1702> (2024, older than 18 months)

negative: the cosign v3.0.0 release notes and changelog were not retrievable, so what cosign does with old flags is unknown.
negative: no source states which clients require `TrustedRoot`, `SigningConfig` or `ClientTrustConfig`.
negative: Kyverno documents no per-entry Fulcio roots field; the `gh` `--tenant` flag is documented nowhere that could be read.
negative: the selection semantics of sigstore-go's `TrustedMaterialCollection` are undocumented in what could be read.

leads: the source of sigstore-go `TrustedMaterialCollection` and `FulcioCertificateAuthority.Verify` — confirms pool semantics and CA validity gating.
leads: the policy-controller `TrustRoot` API source or tests — settles the OR across authorities with different `trustRootRef` values.
leads: the `gh` trusted-root JSONL generator — a worked example of selection by issuer.
leads: the cosign v3 release notes through `gh release view` — settles error, warning or ignore for old flags.
