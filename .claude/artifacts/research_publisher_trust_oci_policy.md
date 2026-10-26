# Research: OCI-native signature policy distribution

## Metadata

- Date: 2026-10-25
- Domain: supply-chain security
- Triggered by: ADR publisher trust distribution (`.agents/discussions/release-signing-trust-default.md`)
- Expires: 2027-04-25
- Axis: registry ecosystems and OCI spec evolution (ecosystem lane). TUF/tuf-on-ci/RSTUF, npm/PyPI/crates/RubyGems/NuGet/Homebrew binding detail, and constant-SAN workflow design are other lanes; they appear here only where they change a conclusion.
- Reading guide: `[S#]` resolves to a link in Sources. Sources published before 2025-04-25 (older than 18 months) are marked OLD there. "(inference)" marks my reasoning, not a source statement. Page text was summarised by the fetch tool; quotes are the tool's quotes of the page, not independent reads of the raw HTML.

## Direct answer

1. **Policy stays consumer-side everywhere.** Notation, containers/image, Flux, Ratify, policy-controller, Kyverno, Connaisseur and Gatekeeper all keep "who may sign what" in a local file or cluster object that the operator supplies out of band (GitOps, Helm, Azure Policy, MDM). None fetches it from the registry that hosts the artifacts. [S1] [S5] [S11] [S14] [S16] [S20] [S22] [S23] [S24]
2. **No OCI-native precedent exists for a registry or catalog publishing the expected signer per repository.** I searched the web and ran `gh search issues` across the sigstore, opencontainers and notaryproject orgs: no proposal, no registry feature. Absence of search hits, not proof. Notation has no "trust policy as OCI artifact" work either; its spec says trust stores are configured out of band, and the nearest open item is [notation#902](https://github.com/notaryproject/notation/issues/902) (fetch certificates, maybe policies, via plugin; open since 2024-03). [S1] [S8]
3. **The only registry-held per-package publisher identity is outside OCI**: PyPI Trusted Publishers (+ PEP 740 attestations) and npm trusted publishing. The registry stores the identity per project, checks it at upload, and consumers trust the registry to deliver it. [S43] [S44] [S45] [S46]
4. **Rotation is solved by set-valued trust plus time evidence, not by a verifier-side "valid until" per identity.** Identity lists are ANY-of everywhere (Notation `trustedIdentities`, Flux `matchOIDCIdentity`, Kyverno attestors, PyPI/npm multiple publishers). Old signatures outlive short-lived or expired certs through RFC 3161 timestamps (Notation, Sigstore TSA) or Rekor integrated time (Sigstore v1). Validity windows exist for trust *infrastructure* (Sigstore `TrustedRoot.valid_for`) and for *keys* (Docker attest `from`/`distrust`, npm key `expires`), not for publisher identities. [S6] [S53] [S55] [S48] [S47]
5. **Revocation is blunt.** Remove the identity or cert and every signature by it fails; Notation says explicitly that a timestamp does not prove a signature predates revocation. Healing means re-signing. [S6] [S22]
6. **Mirrors have two working models.** (a) Copy by digest including referrers: signatures verify unchanged against the upstream signer (`regctl image copy --referrers`, zot `preserveDigest: true`, DHI mirror guide). (b) Re-sign by the mirror operator and replace the policy for that scope (Chainguard private registries sign with the customer's own syncer identity; ECR managed signing signs on push). containers/image adds `remapIdentity` so a mirror path can reuse the vendor's signed identity. [S63] [S38] [S29] [S30] [S40] [S23]
7. **Opt-in verification dies.** Docker Content Trust covered fewer than 0.05% of Docker Hub pulls and is being shut down on 2026-12-08; its cached signing certificates were never refreshed, which made rotation impractical. [S25] [S26]

## Trends

1. **OCI 1.1 referrers are the signature carrier and the spec is frozen at 1.1.** Distribution/image spec 1.1 shipped 2024-02-15; I found no 1.2 release or signature-related working-group change. The tag-schema fallback is client-maintained and the spec itself warns of race conditions and data loss on concurrent updates. [S61] [S62]
2. **Policy engines are consolidating on CEL and rewriting.** Kyverno's `ImageValidatingPolicy` (CEL, attestors for cosign/Notary/TUF) is Stable as of v1.19; Ratify is mid-rewrite (v2 alpha/beta on `ratify-go`, Rego policy "to be ported later"). Both ship new releases weekly; policy distribution is still a Kubernetes concern in each. [S18] [S13]
3. **Identity-based (keyless) replaces key distribution, and the identity pin is moving from mutable names to immutable IDs.** GitHub documents repository-ID checks to survive rename and name reuse; npm binds a trusted publisher to the repository's immutable identity at first publish; Fulcio carries immutable repo and owner ID extensions. [S34] [S46] [S59]
4. **Time evidence is moving from Rekor's own clock to RFC 3161 TSAs.** Rekor v1 integrated time comes from "Rekor's internal clock, which is not externally verifiable" and "is mutable in Rekor without detection"; Rekor v2 clients get a signed timestamp from a separate TSA; cosign's default TSA is valid from 2025-07-04. Notation added TSA support in 1.2 and CRL revocation in 1.3 (2025-01). [S55] [S6] [S7]
5. **Legacy TUF-for-content-trust is being retired.** Docker Content Trust and the Notary v1 service shut down 2026-12-08. Docker's own follow-up experiment, policy distributed through a Docker TUF root with a repo-regex `mapping.yaml` (`docker/attest`, `go-tuf-mirror` mirroring TUF metadata into OCI), is archived (last push late 2024). Helm provenance is still PGP with "decentralized" trust and a page "not yet updated for Helm 4". [S25] [S26] [S48] [S49] [S39]
6. **Registries sign as a service; consumers still verify with their own policy.** ECR managed signing (2025-11-21), DHI keyring, Chainguard registry identities. No registry publishes the expected signer for a repository. [S40] [S28] [S30]
7. **Project velocity differs.** Notation's last CLI release is v1.3.2 (2025-04-27, 18 months ago); Kyverno, Ratify, Connaisseur, cosign, Flux and zot released within the last 30 days (GitHub releases API, retrieved 2026-10-25); policy-controller's last release is v0.15.1 (2026-03-26) and its docs still say "actively under development". [S66] [S14]

## Key findings

### 1. Notation (Notary Project)

- **Policy model.** `trustpolicy.oci.json` holds `trustPolicies[]`, each with `registryScopes`, `signatureVerification {level, override, verifyTimestamp}`, `trustStores` (`ca:`, `signingAuthority:`, `tsa:` types) and `trustedIdentities` (`x509.subject:` DNs or `*`). Exactly one policy applies per artifact (exact repo match first, then global `*`); no match means fail. [S1]
- **Verification levels.** `strict` enforces integrity, authenticity, authentic timestamp, expiry, revocation; `permissive` logs revocation and expiry failures; `audit` enforces integrity only; `skip` disables (not allowed on the global scope). Per-check overrides exist except integrity. [S1]
- **Distribution.** Trust stores are configured "ahead of verification time" by an out-of-band mechanism "outside its scope"; the CLI offers `notation policy import/show` (replace the local file after a prompt). Nothing describes remote or signed policy. [S1] [S5]
- **Multiple signatures.** The verifier tries each signature referrer; a signature failing a filter or check is skipped; verification succeeds as soon as one passes; it fails only when all are exhausted. Dual-signing during an overlap therefore works. [S3]
- **Identity is the cert subject plus a trusted root.** Leaf rotation is invisible to policy while the root and subject stay stable. The spec advises trust-store roots, not intermediates, because rotating intermediates breaks verification. [S1]
- **Plugins.** Plugin capabilities include `SIGNATURE_VERIFIER.TRUSTED_IDENTITY` and `SIGNATURE_VERIFIER.REVOCATION_CHECK`; Notation passes `trustedIdentities` from the policy to the plugin unchanged, so a plugin may define non-x509 identities (the spec page does not say which formats). AWS Signer's installer ships the AWS Signer root cert in the Notation trust store and a configurable trust policy; that its trusted identity is the signing-profile ARN is my recollection of the AWS docs, not read from the page I fetched. [S4] [S41]
- **Revocation.** OCSP preferred, CRL fallback with cache (v1.3.0, 2025-01); enforced by default under `strict`; a cert without OCSP/CRL info is treated as not revoked. [S7] [S1]
- **Replay.** CVE-2024-23332: a compromised registry can serve outdated artifacts and `permissive` policies accept them; recommended fix is `strict` plus signature expiry. [S10]
- **Open gaps.** Multiple trust policies per scope ([#759](https://github.com/notaryproject/notation/issues/759)), remote import of certs ([#902](https://github.com/notaryproject/notation/issues/902)). Both open since 2023/2024. [S8]

### 2. Admission-time and pull-time verifiers

| System | Where policy lives | How it gets there | Override model |
|---|---|---|---|
| Ratify v1.4 | `Verifier` CRD with inline `trustPolicyDoc` (Notation) or cosign `trustPolicies` (scopes, keys, keyless); certs via `KeyManagementProvider` | Kubernetes API (Helm/GitOps); docs do not describe propagation | Scopes may not overlap (validated at verifier creation); tag-based scopes "will NOT match" [S11] [S12] |
| sigstore policy-controller | `ClusterImagePolicy` (image globs, authorities: key / keyless identities / static), `TrustRoot` | CRDs + webhook; namespaces opt in by label | Policies AND, authorities OR; `mode: warn`; `no-match-policy` warn/allow/deny; unmatched images rejected by default [S14] |
| Kyverno `verifyImages` / `ImageValidatingPolicy` | `ClusterPolicy` / `ImageValidatingPolicy` CRs; attestors with `count`, keyless `subject`/`issuer` (+RegExp), `additionalExtensions` for Fulcio OIDs, `roots`, `ctlog`, TUF, Notary `certs`+`tsaCerts` | Kubernetes API (GitOps) | `failureAction` Audit/Enforce; `failurePolicy: Ignore` when registry offline; verification cache TTL 60 min, "any change ... will not be reflected till the cache entry expires" [S16] [S17] [S18] |
| Connaisseur | Helm `values.yaml`: validators (`trustRoots`: key / keyless issuer+subject, Rekor) and ordered image-pattern policy | `helm install/upgrade` | Pattern to validator; multi-signature via `trustRoot: "*"` ("not a real wildcard") [S20] |
| Gatekeeper + Ratify | `Provider` CRD (url, caBundle, timeout) called by `ConstraintTemplate` via `external_data`; Ratify is community-listed | Kubernetes API; template distribution not described on the page | Constraint scope; validation fail-open/closed not described [S21] |
| Flux `OCIRepository.spec.verify` | `provider: cosign` with Secret of `*.pub` keys or keyless `matchOIDCIdentity[]` (issuer+subject regexes, ANY-of); `provider: notation` with Secret holding certs + `trustpolicy.json`; `trustedRootSecretRef` for a custom Sigstore `trusted_root.json` | Per-object spec and Secrets | Older artifacts stay valid "as long as the right key is in the secret"; keyless unsupported behind `proxySecretRef`; offline not documented [S22] |
| containers/image `policy.json` | Transport, scope (most specific wins), requirement list (`sigstoreSigned` with `fulcio` oidcIssuer+subjectEmail+Rekor key, `pki`, keys), `signedIdentity` incl. `remapIdentity` | File in `/etc/containers` or `~/.config/containers` | All requirements must pass; `remapIdentity` replaces a path prefix so a private mirror can reuse the vendor's signed identity [S23] |
| AKS Image Integrity (Ratify) | Azure Policy initiative deploys Ratify; Ratify CRDs hold certs from Key Vault | Azure Policy assignment; verification config still `kubectl apply` | Preview; Notation only; Audit effect only (page updated 2026-05-13) [S24] |

**policy-controller `TrustRoot`.** Three modes: `remote` (initial root plus TUF mirror, kept current automatically), `repository` (serialized air-gapped TUF; "Will not make outbound network connections"; rotation then manual), `sigstoreKeys` (inline CAs, CT logs, Rekor, TSAs). This is trust *infrastructure* distribution, not publisher identity. [S15] [S14]

### 3. Content-trust deployments

- **Docker Content Trust.** Opt-in, TUF + Notary v1; <0.05% of Docker Hub pulls; Docker Official Image signing certs "once cached by the Docker client, are not subsequently refreshed, making certificate rotation impractical"; full shutdown 2026-12-08 after July-August 2026 brownouts. Docker "cannot provide replacement signatures on your behalf". Recommended: Sigstore or Notation by the publisher; Kyverno (cosign) or Ratify+Gatekeeper (Notation) to enforce; DHI as a managed alternative. [S25] [S26]
- **Docker Hardened Images.** Verification with `cosign verify --key https://dhi.io/keyring/latest.pub --experimental-oci11`; the public keys live in `docker-hardened-images/keyring` as `dhi-1.pub` (inactive) and `dhi-2.pub` (active) with a `latest` alias; some attestations skip Rekor because an entry "could expose private information", so verification needs `--insecure-ignore-tlog`. No identity, trust policy or revocation procedure is published. Mirroring requires copying the image and the attestations (`regctl image copy --referrers`, `regsync`). [S27] [S28] [S29]
- **Chainguard.** Public images: `--certificate-oidc-issuer=https://token.actions.githubusercontent.com --certificate-identity=https://github.com/chainguard-images/images/.github/workflows/release.yaml@refs/heads/main` (constant identity on a branch ref). Private or dedicated registries: issuer `https://issuer.enforce.dev`, identity regexp over the organisation's `IMAGE_SYNCER` and `CUSTOM_IMAGE_BUILDER` UIDPs, i.e. the registry re-signs synced content under the customer's own identity. Chainguard has published an advisory titled "Changing Identity on Chainguard Container signatures" (body not retrievable, HTTP 403; title only). Identity is published in docs, not in a machine-readable catalog field I could find. [S30]
- **GitHub artifact attestations.** `actions/attest` with `push-to-registry: true`; `gh attestation verify oci://...` takes `--owner`/`--repo`, `--signer-workflow` (required with `--signer-repo` for reusable-workflow signers), `--cert-identity[-regex]`, `--source-ref/--source-digest`, `--deny-self-hosted-runners`. Offline: `gh attestation download` + `gh attestation trusted-root > trusted_root.jsonl` (TUF; `--tuf-url`, `--tuf-root`, `--verify-only`) + `--bundle` + `--custom-trusted-root`. Public repos use Sigstore public good, private repos GitHub's own instance. Caveats quoted: Sigstore keys rotate "a few times per year"; "You will not know if key material has been revoked since you last generated the trusted root file"; offline `-R` checks only names recorded in the cert, so compare the immutable `sourceRepositoryIdentifier` to defeat rename and name-reuse. [S31] [S32] [S33] [S34]
- **Harbor.** Per-project toggle, "only allow verified images to be pulled"; the page does not say whether it checks signer identity; signatures replicate with the artifact between Harbor instances and are unlinked "coordinating artifacts" on other targets; event-based replication unsupported. [S35] [S36]
- **Zot.** Trust extension verifies with cosign public keys or Notation certs uploaded via API; results (`IsTrusted`) are stored and shown in GraphQL, with no stated blocking of pulls; `trustpolicy.json` is auto-managed and fixed at `strict`. [S37]
- **Helm.** `.prov` + PGP keyring, `helm verify`; "decentralized model"; key distribution left to Keybase-style tools; rotation, revocation, expiry not covered; OCI storage of provenance not described (a `helm-sigstore` plugin is suggested). [S39]
- **ECR managed signing (2025-11-21).** Registry-wide signing configuration of up to 10 rules, each a Signer profile ARN plus repository wildcard filters; ECR signs on push under the pusher's IAM identity; multiple matching rules produce multiple signatures; AWS Signer handles key lifecycle and signatures carry a configurable validity period. Verification is by Notation with a trust store holding the AWS Signer root cert. This is the closest thing to a registry-held "repo to signer" mapping, but it configures signing, not what verifiers expect. [S40] [S41]
- **Google Binary Authorization.** One policy per project; attestors hold one or more public keys; attestations live in Artifact Analysis; dry-run mode; no rotation procedure described; PKIX and PGP keys. [S42]

### 4. Registry-held publisher binding (outside OCI) and signed policy delivery

- **PyPI.** Trusted Publishers are registered to *projects, not users*; removing a maintainer does not remove their publishers; "PyPI will reject non-Trusted Publisher attestations at upload time"; the provenance object's `publisher` records "claims retained by the index during Trusted Publisher authentication". PEP 740 states it "does **not** increase (or decrease) trust in the index itself" and that provenance objects "are *not* immutable, and may change over time". Verifier checks: cert issued by an a-priori trusted authority, signing subject is the publisher identity, statement subject matches the file; transparency-log inclusion time, if checked, must lie within the cert validity. Identity-claim caveat: a GitLab namespace deleted and recreated is "still recognised"; GitHub workflow rename is "indistinguishable from the old file" except for repo-state claims. [S43] [S44] [S45]
- **npm.** Per-package trusted publisher (owner, repo, workflow filename, optional environment) stored by the registry; configs cannot be edited, only deleted and recreated; at most 10 per package; "A new configuration must complete its first successful publish within two days, or it expires. That first publish binds the configuration to the repository's immutable identity." Registry signing keys are served at `/-/npm/v1/keys` with an `expires` field; PGP signatures were replaced by ECDSA on 2023-04-25. [S46] [S47]
- **Docker attest (archived).** Policy and verification keys resolved from a Docker TUF root; `mapping.yaml` at the TUF targets root maps repo-name regexes to policy files; local `mapping.yaml` "take[s] precedence over TUF managed policies"; keys carry `from` and `distrust`; `LocalPolicyDir` for partial offline use. `go-tuf-mirror` mirrors TUF metadata and targets into OCI repositories (`docker/tuf-metadata`, `docker/tuf-targets`). This is the closest OCI-adjacent design to "per-repository expected signer, delivered signed, overridable locally", and it was abandoned. Why is not stated in the sources; Docker's DCT retirement post cites low usage and an unmaintained upstream. (inference: the two are related, not proven.) [S48] [S49] [S25]
- **OPA bundles.** Signed bundles (`.signatures.json`), `keyid` and `scope` supplied out of band, `revision` in the manifest, `persist: true` for offline start, OCI registry as a bundle source (`type: oci`, one layer), and a failed verification "continues using its existing bundle and reports an activation failure". [S50]
- **TUF (generic).** Per-role thresholds, `EXPIRES` ("Clients MUST NOT trust an expired file"), monotonic versions (rollback protection), snapshot hashes (mix-and-match), delegations with path patterns, root rotation chained by threshold signatures of old and new keys. The spec itself says it is generic and "does not describe policy distribution specifically". [S51]
- **Sigstore's own root.** `trusted_root.json` and signing config distributed through a TUF repo run by five keyholders; online signing at least every three days; production publication after staging tests. [S58] [S56]

### 5. Time evidence and rotation: Notation versus Sigstore

| Aspect | Notation | Sigstore (cosign, sigstore-go) |
|---|---|---|
| Identity key | Cert chain to a trust-store root; `trustedIdentities` match subject DN; leaf cert may rotate freely | OIDC identity in a short-lived Fulcio cert (issuer + SAN/extensions); every signature gets a new cert |
| Signing time | `signingTime` is signed but "unauthenticated" under `notary.x509`; authority-attested `authenticSigningTime` under `notary.x509.signingAuthority` | Not in the cert; proven by log entry or timestamp |
| Time proof | Optional RFC 3161 countersignature; spec forbids using it as authentic signing time; policy needs a `tsa` store; `verifyTimestamp` `always` (default) or `afterCertExpiry` | Rekor v1: entry integrated time under the log's signed entry timestamp (clock "not externally verifiable", "mutable ... without detection"); Rekor v2 and cosign default: separate RFC 3161 TSA. Bundles "must include at least one transparency log's signed entry timestamp or an RFC3161 timestamp" |
| Validity rule | Signing time must fall inside the cert validity; without an authentic timestamp every cert in the chain must be unexpired at verification | At least one timestamp inside the cert chain lifetime (PyPI PEP 740 restates the same rule); `TrustedRoot` instances carry `valid_for` and clients must check "both the valid_for time range and the entire certificate chain" |
| Surviving rotation | Old signature verifies after cert expiry iff a valid timestamp exists and the `tsa` store is configured; without a `tsa` store "the signature verification will fail due to certificates expire" | `TrustedRoot` "MUST include any previously used instance"; overlaps must be accepted; a client with an old root fails only on material issued after its snapshot |
| Revocation | OCSP/CRL; a timestamp does **not** show the signature predates revocation, so a revoked cert kills all its signatures | Avoided by design ("short-lived certificates instead"); compromise handled by distrusting an identity or infrastructure instance; no time-aware identity policy in the verifiers surveyed |
| Artifact freshness | Optional signed `expiry`; "artifact is considered expired when current time >= expiry"; CVE-2024-23332 shows why `strict` matters | None in the bundle; freshness is a tlog or policy concern |

Both models separate "signature validity" from "certificate lifetime" with signed time evidence. They differ on revocation: Notation revokes the *cert* (everything it signed), Sigstore has no revocation primitive and relies on identity removal. [S2] [S6] [S10] [S52] [S53] [S54] [S55]

### 6. Mirroring

- **Copy-through.** Digest-preserving copy that includes referrers keeps signatures valid. `regctl image copy` has `--referrers`, `--referrers-src/-tgt`, `--digest-tags`; `crane copy` documents digest retention but says nothing about referrers; zot `preserveDigest: true` "to preserve upstream digests, signatures, and referrers" (default false changes digests and "breaks ... signature verification"), plus `onlySigned` filter and `syncLegacyCosignTags`. Harbor keeps the signature link only between Harbor instances. [S63] [S38] [S36]
- **Trust after copy-through.** The verifier still needs the *upstream* signer in its policy (DHI mirror docs verify only that artifacts were carried over). [S29]
- **Re-sign.** The mirror operator signs with its own identity and the operator policy for that scope accepts only that identity: Chainguard private registries (syncer identity), ECR managed signing (profile per repository filter, multiple matching rules produce multiple signatures). [S30] [S40]
- **Policy rewriting.** containers/image `remapIdentity` (prefix to signed-prefix); Kyverno `verifyImages[].repository` points verification at a signature repository other than the image's. [S23] [S16]
- **Tag-schema fallback caveat.** Where the registry lacks the referrers API, clients maintain an index tag; concurrent writers race. A [GitLab registry design note](https://gitlab.com/gitlab-org/ops/artifact-registry/-/merge_requests/2127) (secondary, from a search snippet) says fallback triggers differ by client (crane vs oras). GHCR's referrers status is not documented in the GitHub packages page I fetched (OCX's own premise that it needs the fallback is not re-verified here). [S61]

### 7. Precedent for per-repository expected signer

| Candidate | Finding |
|---|---|
| Sigstore "trusted publisher for OCI" | No proposal found (searches and `gh search issues --owner sigstore`) |
| Docker Hub verified publisher / Official Images | No signing-policy documentation found; DCT retired, DOI to get "a different image signing solution" (unnamed) |
| GHCR | GitHub documents attestations and `gh attestation verify --owner/--repo`; no registry-published signer record |
| ECR | Signing rules map repo filters to a Signer profile: signer-side only |
| Artifact Registry / Binary Authorization | Per-project policy with attestor public keys: consumer-side, per project |
| Chainguard / Kubernetes / DHI | Identity or key published in documentation or a keyring repo; consumers copy it into policy |
| PyPI, npm (non-OCI) | Yes: registry-held per-project publisher record, enforced at upload, immutable-ID binding at first publish (npm) |

## Matrix

Legend: **Y** satisfied by design; **P** partial or only with caveat; **N** no. Requirement numbers follow the discussion: (1) self-service onboarding honoured without a client release, (2) rotation and revocation without breaking old releases, (3) offline and air-gapped verification, (4) operator override (e.g. a mirror accepting only its own signatures), (5) storage, mutation, freshness and rollback protection of the trust data.

| System | 1 Onboarding | 2 Change safety | 3 Offline | 4 Operator override | 5 Storage and freshness |
|---|---|---|---|---|---|
| Notation | N: policy and trust store are local | P: leaf rotation invisible; TSA outlives expiry; revoked cert kills all | P: local files, CRL cache; revocation needs network unless logged or skipped | Y: replace local policy; levels strict/permissive/audit/skip; one policy per scope | N: out of band; no freshness or rollback; replay of artifacts needs signature expiry |
| Ratify v1 | N | P: ANY-of verifiers; cert stores | P: in-cluster; CRL cache; keyless needs Rekor URL | Y: CRD scopes (non-overlapping) | P: Kubernetes API; no freshness semantics |
| policy-controller | N | P: authorities OR; TrustRoot rotates via TUF mirror, manually when serialized | Y: serialized TrustRoot makes no outbound calls | Y: scopes, `mode: warn`, `no-match-policy` | P: CRDs; TUF only for infra roots |
| Kyverno | N | P: `count` m-of-n, multiple entries; no per-identity validity | P: inline keys and certs; keyless needs Rekor unless ignored; cache TTL 60 min | Y: Audit/Enforce, `failurePolicy: Ignore` | P: CRs via GitOps; stale-by-cache |
| Connaisseur | N | P: multi-signature mode | P | Y: ordered pattern rules in Helm values | P: Helm values |
| Gatekeeper (+Ratify) | N | n/a | n/a | Y: constraints | P: distribution of templates not described |
| Flux | N | P: loops over all keys/certs; ANY-of identities; removal breaks old | P: custom `trusted_root.json` secret; offline undocumented | Y: per object | P: Secrets and CRs |
| containers/image | N | P: requirement lists | Y: local file; keys or Rekor key local | Y: most-specific scope; `remapIdentity` for mirrors | N: local file |
| Docker Content Trust | P: publisher self-signs (TOFU) | N: cached certs never refreshed | N | N | TUF-based, retired 2026-12-08 |
| DHI / Chainguard | N: identity published in docs or keyring | P: DHI `dhi-1` inactive and `dhi-2` active; Chainguard has changed identity once | P: DHI static key, no tlog; Chainguard air-gapped article exists (not readable) | consumer | docs only |
| GitHub attestations | Y if the consumer names owner or repo | P: Sigstore keys rotate a few times a year; use repo ID for rename | Y: bundle + `trusted_root.jsonl`; staleness caveat | consumer flags | TUF for trusted root |
| Harbor / Zot | N | n/a | Y | P: Harbor project toggle (presence only); Zot reports only | Harbor and Zot local |
| Helm provenance | N | N: not covered | Y: local keyring | consumer | none |
| ECR managed signing / AWS Signer | P: registry rules map repos to profile (signer side) | P: Signer-managed keys; configurable validity; revocation status API | P | registry owner | registry configuration |
| Binary Authorization | N | P: attestor holds multiple keys | n/a | Y: per-project policy, dry run | policy resource |
| PyPI / npm trusted publishing | Y (non-OCI): per-project, self-service | P: multiple publishers; delete-and-recreate; offboarding hazard | P: index trusted; provenance mutable | N: registry decides | registry database, enforced at upload |
| Docker attest (TUF policy, archived) | P: `mapping.yaml` repo regex to policy | P: keys with `from` and `distrust` | P: `LocalPolicyDir` | Y: local mapping beats TUF | TUF: expiry, versions, thresholds |
| OPA signed bundles | n/a | n/a | Y: `persist: true` | n/a | `revision`; failed verify keeps old bundle |
| Sigstore TUF root (infra) | n/a | Y: `valid_for` windows, previous instances retained | Y: serialized TUF, `trusted_root.jsonl` | Y: custom TrustRoot or `--trusted-root` | TUF, online signing at least every 3 days |

Reading the matrix: no single system covers all five. Row 1 is satisfied only by registry-held records (PyPI, npm) and, per packaging, by GitHub's consumer-names-the-owner flow. Row 5 is satisfied only by TUF-based or signed-bundle systems. Row 4 is satisfied everywhere that the policy is local. Row 3 is cheap when policy and evidence are local files.

## Patterns

| # | Pattern | Adopters | Satisfies |
|---|---|---|---|
| P1 | Consumer-local policy file | Notation, containers/image, Flux, Zot, Helm | 3, 4 |
| P2 | Cluster-config policy delivered by GitOps, Helm or cloud policy | Ratify, policy-controller, Kyverno, Connaisseur, Gatekeeper, AKS Image Integrity | 4 |
| P3 | Publish the identity in documentation; consumers hard-code it | Kubernetes, Chainguard, DHI keyring | none of 1, 2, 5 |
| P4 | Registry-held per-package publisher binding, enforced at upload | PyPI, npm (non-OCI) | 1 |
| P5 | Owner-scoped trust with immutable IDs | `gh attestation verify --owner`, npm first-publish binding, Fulcio ID extensions | 1 (consumer-side), rename safety |
| P6 | Signed, expiring, versioned trust or policy delivery (TUF, signed bundles) | Sigstore root, Docker attest, OPA bundles, policy-controller remote TrustRoot | 5 |
| P7 | Registry or service signs on the publisher's behalf | ECR managed signing, Chainguard registry, AWS Signer | 1 (signer side) |
| P8 | Signed time evidence decouples signature validity from cert lifetime | Notation TSA, Sigstore Rekor and TSA, PEP 740 | 2, 3 |
| P9 | Set-valued, ANY-of trust entries; rotation is additive | Notation, Flux, Kyverno, policy-controller, PyPI, npm | 2 |
| P10 | Staged enforcement modes (audit, warn, enforce) | Notation levels, policy-controller `mode: warn`, AKS Audit, BinAuthz dry run (Kyverno `failureAction` Audit/Enforce is from my recollection; the fetched pages show Enforce only) | rollout |
| P11 | Mirror by digest with referrers, or re-sign and replace policy, or remap identity | regctl, zot, DHI, Chainguard, ECR, containers/image | 4 |

Anti-patterns with evidence: opt-in verification (DCT, <0.05% of pulls); client-cached trust data without a refresh protocol (DCT certificates); presence-only enforcement (Harbor page does not describe identity checks); report-only verification (zot); offline trust roots with no staleness statement (GitHub warns revocations are invisible).

## Sources

Retrieved 2026-10-25. OLD = published before 2025-04-25. UNDATED = living documentation with no date shown. Where a fetch failed or returned only a title, the entry says so and the claim is stated as title-only.

| ID | Source | Age |
|---|---|---|
| S1 | Notary Project trust store and trust policy spec: [S1] | UNDATED (main branch) |
| S2 | Notary Project signature specification: [S2] | UNDATED |
| S3 | Notary Project signing and verification workflow: [S3] | UNDATED |
| S4 | Notary Project plugin extensibility spec: [S4] | UNDATED |
| S5 | Notation manage trust policy guide: [S5] | UNDATED |
| S6 | Notation timestamping guide: [S6] | OLD (modified 2024-09-03) |
| S7 | Notation v1.3.0 announcement: [S7] | OLD (2025-01-17) |
| S8 | notation issue 902: [S8] | OLD (2024-03-04, open) |
| S9 | Notation releases (v1.3.2, 2025-04-27): [S9] | borderline |
| S10 | CVE-2024-23332 (NVD; fetch returned a stub, details via search snippet): [S10] | OLD (2024) |
| S11 | Ratify Notation verifier: [S11] | current (v1.4) |
| S12 | Ratify cosign verifier: [S12] | current (v1.4) |
| S13 | Ratify releases: [S13] | current (v1.4.8 2026-10-23) |
| S14 | sigstore policy-controller overview: [S14] | UNDATED |
| S15 | policy-controller `trustroot_types.go`: [S15] | UNDATED |
| S16 | Kyverno verifyImages, Sigstore: [S16] | UNDATED |
| S17 | Kyverno verifyImages overview: [S17] | UNDATED |
| S18 | Kyverno ImageValidatingPolicy: [S18] | current (Stable v1.19) |
| S19 | Kyverno verifyImages, Notary: [S19] | UNDATED |
| S20 | Connaisseur cosign validator: [S20] | UNDATED |
| S21 | Gatekeeper external data: [S21] | UNDATED |
| S22 | Flux OCIRepository: [S22] | UNDATED |
| S23 | containers-policy.json(5): [S23] | UNDATED |
| S24 | AKS Image Integrity: [S24] | updated 2026-05-13 |
| S25 | Docker, Retiring Docker Content Trust: [S25] | 2025-07-29 |
| S26 | Docker, DCT retirement and migration guidance: [S26] | 2026-06-16 |
| S27 | Docker Hardened Images, verify: [S27] | UNDATED |
| S28 | DHI keyring repository: [S28] | UNDATED |
| S29 | DHI mirror guide: [S29] | UNDATED |
| S30 | Chainguard, verify with cosign (public and private identities; offset 100000 of the page): [S30]. Identity-change advisory, title only (403): [Chainguard advisory](https://support.chainguard.dev/hc/en-us/articles/40405966185883--Update-Changing-Identity-on-Chainguard-Container-signatures). Air-gapped article, title only (403): [Chainguard air-gapped](https://support.chainguard.dev/hc/en-us/articles/53831163051291-Verify-Chainguard-images-in-an-air-gapped-environment) | UNDATED |
| S31 | GitHub, container image attestations: [S31] | UNDATED |
| S32 | `gh attestation verify` manual: [S32] | UNDATED |
| S33 | `gh attestation trusted-root` manual: [S33] | UNDATED |
| S34 | GitHub, verify attestations offline: [S34] | UNDATED |
| S35 | Harbor, implementing content trust: [S35] | UNDATED |
| S36 | Harbor, sign artifacts: [S36] | UNDATED |
| S37 | zot, verifying signatures (v2.1.0 docs): [S37] | possibly OLD (versioned page) |
| S38 | zot, mirroring (v2.1.18): [S38] | UNDATED |
| S39 | Helm provenance: [S39] | stale (page says not updated for Helm 4) |
| S40 | AWS ECR managed signing: [S40]; announcement [ECR what's new](https://aws.amazon.com/about-aws/whats-new/2025/11/amazon-ecr-managed-container-image-signing) | 2025-11-21 |
| S41 | AWS Signer, image signing prerequisites: [S41] | UNDATED |
| S42 | Google Binary Authorization key concepts: [S42] | UNDATED |
| S43 | PEP 740: [S43] | OLD (date not shown by fetch; treat as 2024) |
| S44 | PyPI, producing attestations: [S44] | UNDATED |
| S45 | PyPI trusted publishers security model: [S45] | UNDATED |
| S46 | npm trusted publishers: [S46] | UNDATED |
| S47 | npm registry signatures: [S47] | UNDATED |
| S48 | docker/attest (archived; last push 2024-12-02 per GitHub API): [S48] | OLD |
| S49 | docker/go-tuf-mirror (archived; last push 2024-11-25): [S49] | OLD |
| S50 | OPA bundle management: [S50] | UNDATED |
| S51 | TUF specification (page reports 1.0.37, modified 2026-10-08): [S51] | current |
| S52 | Sigstore security model: [S52] | UNDATED |
| S53 | Sigstore TrustedRoot proto: [S53] | UNDATED |
| S54 | Sigstore bundle format: [S54] | UNDATED |
| S55 | cosign timestamps: [S55] | UNDATED (TSA valid from 2025-07-04) |
| S56 | cosign custom components and signing config: [S56] | UNDATED |
| S57 | cosign verify: [S57] | UNDATED |
| S58 | sigstore/root-signing: [S58] | UNDATED |
| S59 | Fulcio OID information: [S59] | UNDATED |
| S60 | Kubernetes, verify signed artifacts: [S60] | UNDATED |
| S61 | OCI distribution spec (1.1, 2024-02-15): [S61] | OLD (still current, no 1.2 found) |
| S62 | OCI image and distribution 1.1 release post: [S62] | OLD (2024-03-13) |
| S63 | regclient `regctl image copy`: [S63] | UNDATED |
| S64 | crane copy reference: [S64] | UNDATED |
| S66 | Release dates via GitHub releases API for connaisseur, policy-controller, kyverno, notation, notation-go, ratify, cosign, rekor-tiles, zot, sigstore-go, source-controller, gatekeeper, retrieved 2026-10-25: [S66] | 2026-10-25 |

[S1]: https://github.com/notaryproject/specifications/blob/main/specs/trust-store-trust-policy.md
[S2]: https://github.com/notaryproject/specifications/blob/main/specs/signature-specification.md
[S3]: https://github.com/notaryproject/specifications/blob/main/specs/signing-and-verification-workflow.md
[S4]: https://github.com/notaryproject/specifications/blob/main/specs/plugin-extensibility.md
[S5]: https://notaryproject.dev/docs/user-guides/how-to/manage-trust-policy/
[S6]: https://notaryproject.dev/docs/user-guides/how-to/timestamping/
[S7]: https://notaryproject.dev/blog/2025/announcing-notation-v1-3/
[S8]: https://github.com/notaryproject/notation/issues/902
[S9]: https://github.com/notaryproject/notation/releases
[S10]: https://nvd.nist.gov/vuln/detail/CVE-2024-23332
[S11]: https://ratify.dev/docs/plugins/verifier/notation
[S12]: https://ratify.dev/docs/plugins/verifier/cosign
[S13]: https://github.com/ratify-project/ratify/releases
[S14]: https://docs.sigstore.dev/policy-controller/overview/
[S15]: https://github.com/sigstore/policy-controller/blob/main/pkg/apis/policy/v1alpha1/trustroot_types.go
[S16]: https://kyverno.io/docs/policy-types/cluster-policy/verify-images/sigstore/
[S17]: https://kyverno.io/docs/policy-types/cluster-policy/verify-images/
[S18]: https://kyverno.io/docs/policy-types/image-validating-policy/
[S19]: https://kyverno.io/docs/policy-types/cluster-policy/verify-images/notary/
[S20]: https://github.com/sse-secure-systems/connaisseur/blob/master/docs/validators/sigstore_cosign.md
[S21]: https://open-policy-agent.github.io/gatekeeper/website/docs/externaldata
[S22]: https://fluxcd.io/flux/components/source/ocirepositories/
[S23]: https://github.com/containers/image/blob/main/docs/containers-policy.json.5.md
[S24]: https://learn.microsoft.com/en-us/azure/aks/image-integrity
[S25]: https://www.docker.com/blog/retiring-docker-content-trust/
[S26]: https://www.docker.com/blog/docker-content-trust-retirement-and-migration-guidance/
[S27]: https://docs.docker.com/dhi/how-to/verify/
[S28]: https://github.com/docker-hardened-images/keyring
[S29]: https://docs.docker.com/dhi/how-to/mirror/
[S30]: https://edu.chainguard.dev/chainguard/containers/how-to-use/verifying-chainguard-images-and-metadata-signatures-with-cosign/
[S31]: https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations/using-artifact-attestations-to-establish-provenance-for-builds
[S32]: https://cli.github.com/manual/gh_attestation_verify
[S33]: https://cli.github.com/manual/gh_attestation_trusted-root
[S34]: https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline
[S35]: https://goharbor.io/docs/edge/working-with-projects/project-configuration/implementing-content-trust/
[S36]: https://goharbor.io/docs/edge/working-with-projects/working-with-images/sign-images/
[S37]: https://zotregistry.dev/v2.1.0/articles/verifying-signatures/
[S38]: https://zotregistry.dev/v2.1.18/articles/mirroring/
[S39]: https://helm.sh/docs/topics/provenance/
[S40]: https://docs.aws.amazon.com/AmazonECR/latest/userguide/managed-signing.html
[S41]: https://docs.aws.amazon.com/signer/latest/developerguide/image-signing-prerequisites.html
[S42]: https://docs.cloud.google.com/binary-authorization/docs/key-concepts
[S43]: https://peps.python.org/pep-0740/
[S44]: https://docs.pypi.org/attestations/producing-attestations/
[S45]: https://docs.pypi.org/trusted-publishers/security-model/
[S46]: https://docs.npmjs.com/trusted-publishers
[S47]: https://docs.npmjs.com/about-registry-signatures
[S48]: https://github.com/docker/attest
[S49]: https://github.com/docker/go-tuf-mirror
[S50]: https://www.openpolicyagent.org/docs/management-bundles
[S51]: https://theupdateframework.github.io/specification/latest/
[S52]: https://docs.sigstore.dev/about/security/
[S53]: https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_trustroot.proto
[S54]: https://docs.sigstore.dev/about/bundle/
[S55]: https://docs.sigstore.dev/cosign/verifying/timestamps/
[S56]: https://docs.sigstore.dev/cosign/system_config/custom_components/
[S57]: https://docs.sigstore.dev/cosign/verifying/verify/
[S58]: https://github.com/sigstore/root-signing
[S59]: https://github.com/sigstore/fulcio/blob/main/docs/oid-info.md
[S60]: https://kubernetes.io/docs/tasks/administer-cluster/verify-signed-artifacts/
[S61]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md
[S62]: https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/
[S63]: https://regclient.org/cli/regctl/image/copy/
[S64]: https://github.com/google/go-containerregistry/blob/main/cmd/crane/doc/crane_copy.md
[S66]: https://api.github.com/repos/kyverno/kyverno/releases/latest

Gaps I could not close: Chainguard's identity-change advisory body and air-gapped article (403); Connaisseur's overlap resolution and detection mode (page silent); Gatekeeper constraint-template distribution (page silent); Kyverno PolicyException detail (navigation link only); GHCR referrers support (not documented where I looked); Sigstore `TrustRoot` CRD reference page (404, read the Go type instead); PyPI pending publishers and Homebrew attestations (not fetched).

## Recommendation

**Verdict.** Use the registry-held binding pattern (P4) for onboarding, delivered inside a signed, versioned, expiring index snapshot (P6) for freshness, with set-valued identities (P9) and signed time evidence (P8) for rotation, and the existing `[[trust.policy]]` as the single operator override (P1/P2, single most-specific scope as in containers/image and Notation). Offline falls out of bundle-in-referrer plus cached snapshot plus cached trusted root.

### Mapping to the five requirements

| Req | Mechanism | Adopters |
|---|---|---|
| 1 Onboarding | A per-package record in the index lists the publisher identities; the publisher adds it with the package; clients read it from the index they already fetch, so no client release. The edit to that record is gated exactly like an `owners` edit (human review, already in `ocx-sh/index` CONTRIBUTING). | PyPI Trusted Publishers, npm trusted publishing [S45] [S46] |
| 2 Change safety | Identities are an ANY-of set. Benign rotation (workflow move, new family signer) only adds, so older releases keep verifying; dual-signing overlaps also work (a verifier passes when any one signature passes). Compromise: remove the entry, then re-sign affected releases under a new identity (additive referrers; the signing backfill exists). | Notation, Flux, Kyverno, policy-controller [S3] [S22] [S16] [S14] |
| 3 Offline | Verification input is the Sigstore bundle stored as a referrer (cert, log inclusion, timestamp) plus the cached index snapshot plus the cached `trusted_root` with its `valid_for` windows. Accept and document the staleness window ("you will not know if key material has been revoked since you last generated the trusted root file"). | GitHub offline verification, Sigstore bundle and TrustedRoot [S34] [S54] [S53] |
| 4 Operator override | Longest-prefix scope, one winning policy (no merge), operator tier may replace the index-published identities for its scope or extend them. A mirror: re-sign with the mirror identity and replace for that scope, or carry referrers byte-identically and keep the upstream identity. Scope on the logical package name, so a physical mirror path needs no `remapIdentity`. | containers/image, Notation, Chainguard syncer identity, ECR managed signing [S23] [S1] [S30] [S40] |
| 5 Storage and mutation | The binding lives in the index, the unit that already authorises tag to digest. Integrity: sign the snapshot. Freshness and rollback: a monotonically increasing version the client persists as a high-water mark, plus an expiry; offline or frozen installs downgrade expiry to a warning according to the policy level. This applies the three TUF rules (versions, expiry, thresholds); whether to use TUF itself, tuf-on-ci or RSTUF is the other lane's call. | TUF, Sigstore root, OPA bundle revision and persisted bundle, Docker attest [S51] [S58] [S50] [S48] |

### Specific choices

1. **Ship verification on by default for the `ocx.sh` scope, in audit mode first, then enforce.** Three independent engines ship a warn or audit rollout (policy-controller `mode: warn`, AKS Image Integrity where Audit is the only effect, Notation `audit`/`permissive`), and opt-in verification reached <0.05% in the one deployment that tried it (DCT). The current "no policy means skip with INFO" matches the failure mode. [S14] [S24] [S1] [S25]
2. **Pin the immutable repo and owner IDs in each binding, not only the SAN.** Fulcio carries Source Repository Identifier (1.3.6.1.4.1.57264.1.15) and Owner Identifier (.1.17); GitHub documents the rename and name-reuse hole; npm binds to the immutable repository identity at first publish and PyPI documents the namespace-resurrection weakness for GitLab. The codebase notes that these extensions are unused today. Kyverno `additionalExtensions` and `gh --signer-workflow`/`--source-ref` show that extension matching is the established way to avoid regexes over the SAN. [S59] [S34] [S46] [S45] [S16] [S32]
3. **Answer the open phase-1 identity question by the binding's shape, not by a per-tag SAN.** The binding matches `{issuer, repository ID, signing workflow path, optional ref constraint}`. A constant identity on a protected ref is established practice (Chainguard `release.yaml@refs/heads/main`; Kubernetes' constant service-account identity). Phase-1 signatures made under any identity that such a binding can express stay valid under phase 2; per-tag SANs need a regexp or extension match and are the thing to avoid. [S30] [S60]
4. **Require a binding-validation step at onboarding.** npm's rule (first successful publish within two days binds the config) is the only ecosystem precedent for proving the declared identity can actually sign; the index bot's check that a signature by the declared identity exists for the package's first tag would be the OCX equivalent. [S46]
5. **Time evidence: prefer bundles with an RFC 3161 timestamp.** Rekor v1 integrated time is documented as mutable without detection and not externally verifiable; the Sigstore default is moving to a TSA. Treat a missing timestamp as a policy-level decision, not an error. [S55] [S54]
6. **Give the operator tier verification levels,** copying Notation's vocabulary (`strict`, `permissive`, `audit`, `skip`, skip forbidden on the broadest scope) rather than inventing one. [S1]
7. **Keep `--format json`/error output consistent with the tools users already know:** report which identity matched and which entry of the binding set it came from, as Flux reports `SourceVerified`. [S22]

### Prior-art test against the four designs the owner rejected

The discussion bars re-proposing them "unless prior art independently supports it". Result on this axis:

| Rejected design | Independent support found? |
|---|---|
| Built-in client anchors for the own families | No for publishers. Clients embed anchors for *infrastructure* (Sigstore TUF root, AWS Signer root cert in the Notation installer, Docker TUF root). Stays rejected. |
| Mutable per-package signers list in the index root | Yes in substance: PyPI and npm keep a mutable per-project publisher list in the registry, edited by project owners, enforced at upload. The prior art also shows the guard rails this design needs: immutable-ID binding, edit gating, no-edit-in-place (npm), offboarding review (PyPI). [S45] [S46] |
| Append-only signer entries with validity windows | Only for infrastructure and keys (Sigstore `TrustedRoot.valid_for`, "MUST include any previously used instance"; Docker attest `from`/`distrust`; npm key `expires`). No verifier policy surveyed attaches a validity window to a publisher identity. Use ANY-of sets plus removal-and-re-sign. A `distrust_after` field (reject signatures timestamped after T) would be the one element without identity-level precedent. (inference: sound if the time comes from a TSA or log, since an attacker with a stolen identity cannot obtain an earlier timestamp.) Decide it explicitly in the ADR rather than adopting it by default. [S53] [S48] [S47] |
| Recording the verifying signer in `ocx.lock` | No. None of the verifiers surveyed records the verifying signer in a lock artefact. PEP 740 stores the publisher in the *index's* provenance object, not in the consumer's lock. Stays rejected on this axis. [S43] |

### What this research does not settle

- **Rollback protection for policy data is rare.** None of Notation, containers/image, Flux, Kyverno or Connaisseur documents it for their policy; only TUF-based systems (Sigstore root, Docker attest) and OPA's `revision` do. OCX would be ahead of the verifier field; the TUF spec is the reference semantics. [S51] [S50]
- **Time-bounded distrust** (above) has no identity-level precedent.
- **The 8-candidate verification cap** interacts with re-signing and with tag-schema fallback races on registries without the referrers API; the OCI spec gives no server-side fix for the fallback. [S61]
- **Chainguard's identity change** is a real-world example of a signer-identity migration; its content could not be read, so the migration mechanics (dual signing? announcement window?) are unknown here.
