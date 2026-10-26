# Research: Trust frameworks for multi-publisher repositories

## Metadata

- **Date:** 2026-10-25
- **Domain:** supply-chain security
- **Triggered by:** ADR publisher trust distribution (`.agents/discussions/release-signing-trust-default.md`)
- **Expires:** 2027-04-25
- **Axis:** design-pattern precedent (trust frameworks); role: researcher, focus: ecosystem
- **Method:** primary sources (specs, PEPs, TAPs, project docs, source) fetched 2026-10-25. Live Sigstore TUF metadata was downloaded and parsed locally. The GitHub API rate limit was hit late in the session, so a few code searches (for example, `sigstore-oidc` in `tuf-on-ci`) were not run.
- **Age flag:** `[STALE]` marks a source older than about 18 months (cutoff 2025-04-25). Dates come from the page itself; "undated" means the page shows none. Stale sources were kept where they are the only primary source for a design.
- **Convention:** matrix cells are analysis derived from findings `F1`..`F18`; every finding carries its URLs. Statements marked *inference* are mine, not a source's.

## Direct answer

1. **No production system distributes per-package keyless signer identities through TUF.** The nearest real deployments each miss one half:
   - Sigstore's trust-root TUF repo (tuf-on-ci) distributes service keys and one delegated third-party role (npm), not publisher identities (F10).
   - Datadog distributes org-internal developer keys plus an in-toto layout, all inside one company (F9).
   - Per-publisher offline keys at registry scale were tried and failed: PEP 480 is still Draft after 12 years and Docker Content Trust is being shut down on 2026-12-08 (F6, F7).
   - Trusted publishing (npm, PyPI, crates.io) stores the identity binding at the registry and trusts the registry to serve it honestly (F6, F14).
2. **Where TUF does fit OCX is the index, not the publishers.** The minimum security model (PEP 458 shape, Rust RFC 3724 shape, tuf-on-ci in practice) signs the index with an offline root and online snapshot/timestamp keys. It answers host/CDN/git compromise, rollback, freeze, mix-and-match, and gives clients one embedded anchor that rotates by root chaining without a client release (F1, F10, F13).
3. **Root chaining is the only mechanism in the survey that rotates the anchor without a client release.** A single signing key or a CI identity pinned in the binary has no chain; Sigstore's own clients embed a TUF root for this reason (F1, F10).
4. **TUF has no historical key validity.** A delegated key rotation means the delegator swaps the key and the delegated metadata is re-signed; old targets stay valid only because they are re-listed (F2). Time-bounded acceptance via signed time exists in the X.509/Sigstore world (Sigstore TrustedRoot `valid_for`, TAP 18, Notation), not in TUF itself (F17).
5. **Freshness collides with offline-first.** Sigstore's timestamp role expires in 7 days. Every offline-capable client in the survey resolves this by using cached trust material with an explicit staleness policy, never silently (F11, F16).
6. **Operator override is a client-side anchor swap**, not a TUF feature: cosign `--mirror/--root`, gh `--tuf-url/--tuf-root`, Go `GOSUMDB`, APT `Signed-By`, Notation trust policy. TUF's native form is TAP 4 mapping metadata (F4, F15).
7. **Publisher CI compromise is covered by no trust framework in the survey except in-toto layouts** (Datadog). Everything else narrows the identity (workflow ref, environment, immutable repository ID) and relies on detection (F9, F14).
8. **TAP 18 (Fulcio identity as a TUF key) is not a usable path for OCX today:** Draft since 2021, last edited 2023, experimental in securesystemslib and tuf-on-ci, absent from `tough` (F3, F13).
9. **Recommendation (detail in the last section):** a minimum-model TUF repository over the index (offline root and targets, online snapshot/timestamp, one online delegated role for bot-written files), the `tough` client OCX already carries transitively, trusted-publisher-style signer binding inside the index using immutable IDs, and Sigstore verification of artifacts unchanged. Do not give publishers TUF keys. Do not adopt TAP 18.

## Trends

1. **Keyless identity replaced per-publisher offline keys.** PyPI dropped PGP signatures in 2023 and shipped PEP 740 attestations on Trusted Publishing; Docker is retiring DCT and points publishers at Sigstore or Notation (F6, F7). Source ages: PyPI 2024 [STALE], Docker 2025-2026.
2. **TUF survives as operator-signed metadata, not as developer-signed metadata.** The live deployments are Sigstore root-signing, GitHub attestations, and the Rust RFC 3724 design, all minimum-model with an operator-held online key (F10, F11, F13).
3. **Sign the index, not the artifacts.** Rust RFC 3724 signs the crates.io index because it already holds artifact hashes. The 2026 Rust mirroring goal drops artifact-level TUF because of 250,000 artifacts and 3-5 updates per minute (F13).
4. **Automation of TUF operation matured.** tuf-on-ci (git + CI signing events) runs Sigstore's root; RSTUF reached 1.0 on 2025-08-29 (F10, F12). Neither is a wide adoption signal for registries: PEP 458 is still not confirmed in production on PyPI (F6).
5. **Lockfiles are becoming the place for expected identities.** PEP 751 `pylock.toml` has `attestation-identities`; PyPI's roadmap lists TOFU of identities and expected identities in lockfiles; Go's `go.sum` plus the checksum DB is the long-standing precedent (F15, F16).
6. **Immutable IDs over names** is now stated guidance (OpenSSF, GitHub CLI) after rename and resurrection attacks (F14, F11).
7. **Client format compatibility is the recurring operational cost.** Sigstore's 2024 root bump broke older cosign and the Rust client; DCT's cached certificates could not be refreshed (F7, F18).
8. **Transparency logs moved from "one global log" to "policy with witnesses".** Sigsum ships immutable named policies bundled with the tool (F15).

## Key findings

### F1. TUF core mechanics (spec 1.0.37, modified 2026-10-08)

Source: [TUF specification](https://theupdateframework.github.io/specification/latest/)

- **Roles:** root (keys for all top-level roles, kept offline), targets (signs target metadata, can delegate, and delegations are revoked by signing new metadata without them), snapshot (versions of all targets metadata), timestamp (signs the snapshot hash, re-signed often, first file clients fetch).
- **Thresholds:** positive integer; each key counts once.
- **Root rotation by chaining (§5.3, §6.1):** version N+1 must be signed by a threshold of N's keys and a threshold of its own keys, and the version must be exactly N+1. A client holding root N walks forward to the newest root, so an embedded root keeps working across rotations.
- **Fast-forward recovery (§5.3 step 11):** when timestamp or snapshot keys rotate, the client deletes its trusted timestamp and snapshot files.
- **Freshness (§5.1, §5.4-5.6):** the update start time is fixed once ("effectively pause time"), and every role's `expires` is compared to it; expired metadata is rejected as a possible freeze attack.
- **Rollback and mix-and-match (§5.3-5.6):** versions may not decrease, timestamp must increase, snapshot hashes must match the timestamp listing, targets hashes must match the snapshot listing.
- **Consistent snapshots (§6.2):** metadata is written as `VERSION.FILENAME`, targets as `HASH.FILENAME`, so offline signers need not re-sign on every update.
- **Delegations (§4.5, §5.6.7):** ordered, `terminating` stops later matches, `paths` globs or `path_hash_prefixes` (hashed bins), and the client checks a target against the trusted paths of every role in the chain. The search is a pre-order depth-first walk with an application-defined maximum number of roles.
- **`succinct_roles` (TAP 15 hashed bins) is not in the 1.0.37 spec text I fetched**, and python-tuf's docs say setting it "is not a TUF specification feature yet" and yields non-compliant metadata ([python-tuf supporting classes](https://theupdateframework.readthedocs.io/en/latest/api/tuf.api.metadata.supporting.html)). Bins in portable form are `path_hash_prefixes`.

### F2. How a delegated target set behaves when a delegated key rotates

Sources: [TUF spec §3.1.2.1 and §6.1](https://theupdateframework.github.io/specification/latest/), [TAP 8](https://github.com/theupdateframework/taps/blob/master/tap8.md), [TAP 20](https://github.com/theupdateframework/taps/blob/master/tap20.md)

- Base TUF: "Delegated target roles are authorized by the keys listed in the directly delegating target role", and "To replace a delegated developer key, the role that delegated to that key just replaces that key" in its signed metadata. Metadata signed only by the old key then fails verification. Old targets stay valid **only if the delegated metadata is re-signed with the new key and still lists them** (inference from the two quoted rules). TUF has no per-target or per-key validity window.
- TAP 8 (Accepted, v2 modified 2024-03-26 [STALE]): the delegatee rotates its own keys with `ROLE.rotate.VERSION` files signed by a threshold of the old keys, versions increasing by exactly 1, no expiry, listed in snapshot. The delegator can override by delegating to a new role name. Not usable for timestamp or snapshot.
- TAP 20 (Accepted, 2024-01-29 [STALE]): self-revocation by rotating a role to a null key; the role stays invalid until re-delegated under a new name and key. Motivation: community repositories where volunteer offline-key holders take hours or days to act.

### F3. TAP 18: Fulcio identity as a TUF key

Sources: [TAP 18](https://github.com/theupdateframework/taps/blob/master/tap18.md) (Draft, version 0, created 2021-07-27, last modified 2023-04-06 [STALE]), [securesystemslib SigstoreKey](https://github.com/secure-systems-lab/securesystemslib/blob/main/securesystemslib/signer/_sigstore_signer.py), [tuf-on-ci online signing](https://raw.githubusercontent.com/theupdateframework/tuf-on-ci/main/docs/ONLINE-SIGNING-SETUP.md)

- Mechanism: keytype `sigstore-oidc`, scheme `Fulcio`, `keyval` holds `identity` and `issuer`; signatures carry a `bundle` (certificate plus Rekor evidence). Verification is offline: chain to the Fulcio root, identity and issuer match, inclusion proof valid, Rekor SET inside the certificate validity.
- Compromise handling is "use existing TUF processes for revocation": the delegator replaces the metadata naming the compromised account.
- Compatibility: clients that do not recognise Fulcio certs "will not be able to validate signatures from Fulcio certs, but will be able to parse the metadata".
- Implementation: reference is go-tuf PR #181; the only maintained implementation found is `SigstoreKey` in securesystemslib, whose docstrings call the format "not yet considered stable" and which calls `Verifier.production()` (public-good Sigstore only). tuf-on-ci offers Sigstore online signing marked "currently experimental" and "not supported by all TUF client libraries". A GitHub code search for `sigstore-oidc` found no hit in python-tuf or go-tuf on 2026-10-25 (search is non-exhaustive).
- Rust: `tough` key types are RSA, Ed25519, ECDSA only (F13).

### F4. TAP 3, TAP 4, TAP 13: AND-delegation and client-side repository selection

Sources: [TAP 3](https://github.com/theupdateframework/taps/blob/master/tap3.md) (Accepted, 2019 [STALE]), [TAP 4](https://github.com/theupdateframework/taps/blob/master/tap4.md) (Accepted, 2020-06-24 [STALE]), [TAP index](https://github.com/theupdateframework/taps) (current status list)

- TAP 3 multi-role delegations: a delegation can require `min_roles_in_agreement` roles to publish identical hashes and length (an AND relation). The example is release engineering and QA both signing.
- TAP 4 mapping metadata (`map.json`): a client-side, priority-ordered list of path patterns to repositories, with `terminating` and a `threshold` of repositories that must agree. The file "is only modified by a user who is trusted to configure the updater instance". Motivating case: an enterprise routes selected paths to a private repository and everything else to a public one, or requires both to sign.
- TAP 13 (user selection of top-level targets via mapping metadata) is Draft. This is TUF's native answer to operator override (requirement 4).
- TAP 19 (content-addressable systems) is Draft (2023-04-25 [STALE]); its motivation cites Git, IPFS and OSTree, not OCI ([TAP 19](https://github.com/theupdateframework/taps/blob/master/tap19.md)).

### F5. Statuses in the TAP index (fetched 2026-10-25)

Source: [TAP index](https://github.com/theupdateframework/taps)

Accepted: 3, 4, 6, 8, 9, 10, 11, 12, 15, 20, 21 (ML-DSA). Draft: 13, 16 (Snapshot Merkle trees), 17, 18, 19. Deferred: 14. Rejected: 5, 7. No Final section exists.

### F6. PEP 458, PEP 480, PEP 740 (PyPI)

Sources: [PEP 458](https://peps.python.org/pep-0458/) (Accepted; created 2013, last modified 2025-02-01 [STALE]), [PEP 480](https://peps.python.org/pep-0480/) (Draft; page modified 2026-10-23, body text dates from 2014-2020 [STALE]), [PEP 740](https://peps.python.org/pep-0740/) (Final, resolved 2024-07-17 [STALE]), [Discourse status thread](https://discuss.python.org/t/pep-458-current-status-and-next-steps-feedback-requested/17211) (2022-2023 [STALE]), [Warehouse #5247](https://github.com/pypa/warehouse/issues/5247) (closed 2022 [STALE]), [EuroPython 2024 abstract](https://programme.europython.eu/europython-2024/talk/GGWUNA) [STALE]

- **PEP 458 design (minimum security model):** root, timestamp, snapshot, targets; targets delegate to `bins` then 16,384 `bin-n` roles by SHA2-512 hash. Online keys (one shared key) for timestamp, snapshot and all `bin-n`; offline keys for root (threshold at least 2), targets and bins. Expiry: root/targets/bins one year, timestamp/snapshot/`bin-n` one day. It protects against mirror and CDN compromise "but not against a compromise of PyPI itself".
- **PEP 458 compromise recovery:** revoke the affected keys through root, audit `bin-n` targets against the last known good snapshot, bump versions, issue a new timestamped snapshot; a lost root threshold means out-of-band root replacement.
- **PEP 480 (maximum model):** developers sign with offline keys; `claimed` (offline), `recently-claimed` (online, for new projects), `unclaimed` (renamed `bins`). The PEP names developer key management as a major obstacle and still cites MiniLock-style derived keys; it does not mention Sigstore or TAP 18. It requires PEP 458 to land first.
- **Status:** the latest evidence is a 2024 talk abstract saying PEP 458 "has not made its way into production yet". The Discourse thread shows implementation stalled on review capacity and a pivot to RSTUF. I found no source confirming a production deployment as of 2026-10 (absence of evidence, not proof).
- **PEP 740 (attestations):** the index verifies Sigstore-based attestations at upload against the Trusted Publisher identity; the PEP "does not preclude or exclude" PEP 458/480 and keeps the index trusted ("a dishonest index could also alter or omit attestations"). Trail of Bits states the same: "downstream clients still need to trust PyPI itself to serve attestations honestly" ([Trail of Bits](https://blog.trailofbits.com/2024/11/14/attestations-a-new-generation-of-signatures-on-pypi/), 2024-11-14 [STALE]).
- **Why PEP 740 shipped instead:** no source states the reason. Observable differences (inference): it needs no developer key management, no client change to land, and no new infrastructure beyond upload verification. The 2021 Warehouse thread debated TUF against transparent logs and Sigstore. A SigstoreCon 2024 abstract lists "Using TUF to distribute complex identity policies" as a future idea ([abstract](https://sigstoreconday2024.sched.com/event/1n4pO/the-next-5-years-of-supply-chain-security-on-pypi-william-woodruff-trail-of-bits), 2024 [STALE]).
- **Adoption:** over 20,000 attestations, just over 5% of the top 360 projects ([Sigstore GA post](https://blog.sigstore.dev/pypi-attestations-ga/), 2024-11-14 [STALE]).

### F7. Notary v1 / Docker Content Trust

Sources: [Docker: Retiring DCT](https://www.docker.com/blog/retiring-docker-content-trust/) (2025-07-29), [Docker: retirement and migration guidance](https://www.docker.com/blog/docker-content-trust-retirement-and-migration-guidance/) (2026-06-16), [InfoQ](https://www.infoq.com/news/2025/08/docker-content-trust-retired) (2025-08), [DCT overview](https://docs.docker.com/engine/security/trust/), [DCT delegations](https://docs.docker.com/engine/security/trust/trust_delegation/) (both undated)

- **Timeline:** retirement announced 2025-07-29; write brownouts 2026-07-14/15, read brownouts 2026-08-10/12; **full shutdown of `notary.docker.io` 2026-12-08**.
- **Stated reasons:** under 0.05% of Docker Hub pulls used DCT; upstream Notary unmaintained; the oldest Official Images certificates began expiring 2025-08-08 and "Docker Client caches these certificates and does not refresh them, so rotation is impractical". Replacement guidance: Cosign or Notation, signatures stored as OCI artifacts.
- **Infrastructure and portability (InfoQ):** Notary server, signer, client and a MySQL database with mutual TLS; signing data was lost when images moved to registries without a Notary server. OCX copies signature referrers verbatim, which is the opposite property.
- **Key management UX (Docker docs):** losing the root key is unrecoverable; losing any other key needs a Docker Hub support ticket and "every consumer of a signed tag from that repository must intervene manually".
- **Delegations (Docker docs):** `targets/releases` is the canonical tag source. Rotation is "publish a new key under the same signer name", then remove the old one with the Notary CLI. **Revocation: `docker trust signer remove` removes the signer and "tags they signed must be re-signed by an active delegation".** This is the exact failure for requirement 2: removing a signer invalidates older releases until someone else re-signs them.
- Docker's blogs do not attribute the low adoption to key management; that attribution is not sourced.

### F8. Uptane (director plus image repository)

Source: [Uptane Standard 2.1.0](https://uptane.org/docs/2.1.0/standard/uptane-standard) (undated)

- Image repository: human-controlled, rarely updated, holds binaries and signed metadata, supports delegations. Director repository: automated, online, signs per-vehicle instructions from a private inventory.
- Threat model: an attacker may compromise either repository but not both; ECUs verify that both Targets metadata agree (full verification) or at least check the Director (partial verification).
- Delegated roles can require a threshold of agreeing signers; repository mapping metadata supports more than two repositories. Supply-chain compromise and malicious mirrors are out of scope; the standard says it complements in-toto.
- Relevance (inference): the two-repository agreement is TAP 4 with fixed roles. A private mirror acting as "director" (what this fleet may install) over the public index as "image repository" (what exists) is the same shape as requirement 4, at the cost of two repositories to run.

### F9. in-toto plus TUF: ITE-2, ITE-3, Datadog

Sources: [ITE index](https://github.com/in-toto/ite) (lists ITE-2 as the TUF+in-toto overview, ITE-3 as the Datadog case study), [ITE-3](https://github.com/in-toto/ite/blob/master/ITE/3/README.adoc) (created 2019-07-19 [STALE]), [Datadog blog](https://www.datadoghq.com/blog/engineering/secure-publication-of-datadog-agent-integrations-with-tuf-and-in-toto) (2019-06-03 [STALE])

- Roles split: TUF distributes the root of trust, the in-toto layout and the functionary keys; in-toto verifies that the CI pipeline packaged the source a developer signed. Inspection extracts the wheel and compares to the developer-signed source, so "a compromised pipeline" cannot make users trust wheels "with source code that was never released by Datadog developers".
- Parameters (ITE-3): `tag` step uses (1, n) YubiKeys, n being the developer count, "so that any developer may release a new integration at any time"; builder and signer steps use (1,1) online keys; the root layout uses (2,3) offline keys, expires in 1 year.
- TUF configuration: `wheels-signer` delegates terminating by first letter to 26 roles `wheels-signer-[a-z]`, each (2,3) offline, 1-year expiry; those delegate to online (1,1) roles with **1-week expiry, "the maximum lifetime allowed for a release of a wheel"**. In-toto metadata uses 16 hex-prefix delegations.
- Lessons: delegation is used mainly "to split metadata bandwidth costs"; short expiry deliberately ages releases out, which OCX cannot copy (old releases must stay valid); the blog does not say whether verification works offline or the root is pinned.
- Adoption scope: one organization, 100+ integrations (ITE-3), no self-service third-party publishers.

### F10. tuf-on-ci and the Sigstore root-signing repository (live, closest to a git-backed index)

Sources: [tuf-on-ci README](https://github.com/theupdateframework/tuf-on-ci), [REPOSITORY-MAINTENANCE](https://raw.githubusercontent.com/theupdateframework/tuf-on-ci/main/docs/REPOSITORY-MAINTENANCE.md), [SIGNER-MANUAL](https://raw.githubusercontent.com/theupdateframework/tuf-on-ci/main/docs/SIGNER-MANUAL.md), [DELEGATION-MANUAL](https://raw.githubusercontent.com/theupdateframework/tuf-on-ci/main/docs/DELEGATION-MANUAL.md), [ONLINE-SIGNING-SETUP](https://raw.githubusercontent.com/theupdateframework/tuf-on-ci/main/docs/ONLINE-SIGNING-SETUP.md), [sigstore/root-signing](https://github.com/sigstore/root-signing), live metadata [root](https://raw.githubusercontent.com/sigstore/root-signing/main/metadata/root.json), [targets](https://raw.githubusercontent.com/sigstore/root-signing/main/metadata/targets.json), [snapshot](https://raw.githubusercontent.com/sigstore/root-signing/main/metadata/snapshot.json), [timestamp](https://raw.githubusercontent.com/sigstore/root-signing/main/metadata/timestamp.json), [registry.npmjs.org](https://raw.githubusercontent.com/sigstore/root-signing/main/metadata/registry.npmjs.org.json) (all parsed 2026-10-25), [Sigstore talk abstract](https://sigstoreconday2024.sched.com/event/1n4pX/rewriting-root-signing-a-deep-dive-into-sigstore-trust-root-delivery-jussi-kukkonen-google) (2024-11-12 [STALE])

**What it is.** "A secure artifact delivery system that operates on a Continuous Integration platform": a TUF repository implementation plus a local signing tool with hardware-key (YubiKey) support, guided signing events, delegations with thresholds, and online signing via Google Cloud, Azure, AWS KMS or Sigstore. The README claims "even a fully compromised repository hosting will not lead to compromised downloader clients" and says it is best for "low to moderate frequency of change" in artifacts and keys. Deployments listed: Sigstore `root-signing` and `root-signing-staging`, GitHub's artifact attestation repository, a demo.

**How a signing event works (git-driven).**
- Pushing a `sign/*` branch opens a signing-event PR; the PR reports status and notifies signers. Signers run `tuf-on-ci-sign <event>` locally; "trust only your local signing tool, not the PR reports". Signatures are committed to the event branch.
- Artifacts are plain files under `targets/`; files in `targets/<role>/` belong to that delegated role (up to four directory levels).
- Workflows (root-signing README): `create-signing-events` (scheduled, opens PRs near expiry), `signing-event` (on `sign/*`), `online-sign` (on main pushes and near online-signature expiry, "at least every three days"), `publish` (GitHub Pages `publish` branch). A staging repo tests non-trivial changes; a preproduction deployment runs client tests before production.
- Adding or removing a signer, changing a threshold, or creating a delegated role is itself a signing event; the delegating role's signers sign the new delegation.
- Online delegation: `online-sign-targets` signs a delegation's metadata with a KMS key; **one online signer per delegation, online and offline signers cannot be mixed, feature is "experimental"**; the flow opens the PR as draft, signs, then marks it ready.

**Live configuration of the Sigstore repository (parsed from raw metadata, 2026-10-25).**

| Item | Value |
|---|---|
| Root | version 16, threshold 3 of 5 (ECDSA P-256, hardware keys of named individuals), expires 2027-04-22, expiry period 197 d, signing starts 46 d before |
| Targets | version 14, threshold 3 of 5 offline keys, expires 2036-05-09, expiry period 3650 d, signing period 31 d |
| Snapshot | version 166, one online key (GCP KMS), expires 2036-10-04, expiry 3650 d, signing period 365 d |
| Timestamp | version 817, same KMS key, expires 2026-11-01, **expiry 7 d, signing period 6 d** |
| Delegations | exactly one: `registry.npmjs.org`, paths `registry.npmjs.org/*`, terminating, threshold 1, one ECDSA key held in Azure Key Vault (`npm-tuf-delegate-2024-08`), role expiry 182 d, signing period 31 d, target `registry.npmjs.org/keys.json` |
| Targets published | `trusted_root.json`, `signing_config*.json`, Fulcio/Rekor/CT keys and certificates |
| Keyholders | five named individuals from different organizations; one joined 2025-07, two retired (README) |

Reading (inference): freshness is bought only by the 7-day timestamp; snapshot and targets are long-lived so history does not expire. A third party (npm) is onboarded as a path-scoped terminating delegation with an online key. Replacing that key (the dated key name shows it was rotated in 2024-08) is a change to `targets.json`, which needs three offline signatures. The delegation costs a root-quorum event per rotation.

**Cost datapoints.** Root went through 16 versions since the June 2021 ceremony ([README keyholder dates](https://github.com/sigstore/root-signing)). The 2024 talk abstract says the project "moved to tuf-on-ci as tooling" after "past incompatibility incidents" (F18).

### F11. GitHub attestations and offline verification

Sources: [GitHub blog: Artifact Attestations](https://github.blog/security/supply-chain-security/introducing-artifact-attestations-now-in-public-beta/) (2024-05-02 [STALE]), [gh offline verification](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline) (undated), [gh attestation trusted-root manual](https://cli.github.com/manual/gh_attestation_trusted-root)

- GitHub runs its own Fulcio and timestamp authority with the trust root "managed via a quorum of hardware tokens"; TUF delivers the trust root at `tuf-repo.github.com`, cached under `~/.sigstore/root/tuf-repo.github.com`. The README of tuf-on-ci lists GitHub as a deployment.
- Offline: `gh attestation download` (online), `gh attestation trusted-root > trusted_root.jsonl` (online), then `gh attestation verify --bundle ... --custom-trusted-root ...` offline. The page states the file "has no built-in expiration", material signed after generation verifies only until the next key rotation, and "You will not know if key material has been revoked since you last generated the trusted root file."
- Rename and reuse: offline `-R` checks only owner/repo names in the certificate, which can be stale; the page tells verifiers to compare the certificate's `sourceRepositoryIdentifier` with the repository ID from the API.
- Custom mirror: `--tuf-url` plus an out-of-band `--tuf-root`.

### F12. RSTUF (Repository Service for TUF)

Sources: [RSTUF 1.0.0](https://repository-service-tuf.readthedocs.io/en/latest/blog/posts/rstuf-v1.0.0-is-out.html) (2025-08-29), [usage guide](https://repository-service-tuf.readthedocs.io/en/latest/guide/general/usage.html), [deployment guide](https://repository-service-tuf.readthedocs.io/en/latest/guide/deployment/index.html)

- First stable release 2025-08-29, "ready for production use"; root signing by PEM, YubiKey or Sigstore; online keys in AWS/GCP/Azure KMS, Vault or container secrets; hash-bin delegations (online) and custom delegations; API-first with async artifact add/remove; guided "Metadata Update" ceremony for key add/remove, threshold and root-expiry changes, including compromise recovery. Hash bins inside custom delegations are "planned".
- Shape: container images (Helm, Docker, Kubernetes manifests) with a message broker (Redis or RabbitMQ) and Postgres, per the deployment page. It is a service, not a static-file git workflow. The docs name no production adopters. A 2024 EuroPython abstract calls it mature enough to start incremental Warehouse and RubyGems integrations [STALE]; no completed rollout found.

### F13. Rust ecosystem: RFC 3724, mirroring goal, client libraries, what OCX already has

Sources: [RFC 3724 PR](https://github.com/rust-lang/rfcs/pull/3724) (open, unmerged, opened 2024-10-31 [STALE], updated 2026-10-02), [rendered text](https://github.com/walterhpearce/rfcs/blob/tuf/text/3724-theupdateframework.md), [2026 mirroring goal](https://goals.rust-lang.org/2026/mirroring.html), [tough](https://github.com/awslabs/tough) and [CHANGELOG](https://github.com/awslabs/tough/blob/develop/tough/CHANGELOG.md), [rust-tuf](https://github.com/theupdateframework/rust-tuf), [sigstore-rs Cargo.toml](https://github.com/sigstore/sigstore-rs/blob/main/Cargo.toml)

- **RFC 3724 design:** two repositories. `tuf-root`: root 5-of-9 quorum (Project leadership selects members), release roles, and the signature over the `tuf-crates` root. `tuf-crates`: the crates.io **index** is the target, not the artifacts, "because the index contains SHA-256 hashes"; root is the crates.io team with threshold 3, re-signed by the `tuf-root` quorum on membership change; one top-level targets role held as a **single online key (AWS KMS)** for automated publishing. "Crate, Organization or Namespace-level Trusted Authors" via delegation is explicitly future work. TAP 16 (Snapshot Merkle trees) is proposed for scale.
- **RFC status:** supersedes the earlier non-TUF PKI RFC #3579; still open after 24 months. The 2026 goal ("Implement Verifiable Mirroring Prototype", contact walterhpearce) plans "a first trial pass of RFC #3724, with modifications", practising key ceremonies on the rustup repository first, and rejects direct artifact signing on scale grounds (250,000 artifacts, 3-5 updates per minute).
- **`tough`** (AWS Labs, Apache-2.0 or MIT): client library plus `tuftool`; latest `tough-v0.24.0` released 2026-07-10 (0.23.0 on 2026-06-18, breaking: chrono to jiff). Schema has `terminating` and `path_hash_prefixes` delegations; **no `succinct_roles`** (the only text match is a doc comment), key types RSA, Ed25519, ECDSA only (no `sigstore-oidc`). No TAP 8/20 rotate-file support found in the schema listing.
- **`tuf` crate (rust-tuf)**: version `0.3.0-beta15`, edition 2024; its README says "Beta Software... may not suitable for production use... the API is unstable". Commits continue (latest 2026-08-24). Delegations are implemented (`terminating`); no `succinct_roles` found.
- **What OCX already has:** `crates/ocx_sign/Cargo.toml` enables `sigstore-trust-root`, which pulls `tough` "for the embedded Sigstore root"; `Cargo.lock` carries `tough 0.22.0` (the fork pins `0.22`, upstream sigstore-rs 0.14.0 uses `0.23`). A Rust TUF client is therefore already in the binary; using it for a second repository adds no new dependency family.

### F14. OpenSSF Securing Software Repositories WG

Sources: [Principles for Package Repository Security](https://repos.openssf.org/principles-for-package-repository-security.html) (v0.1, Feb 2024 [STALE]), [Trusted Publishers for All Package Repositories](https://repos.openssf.org/trusted-publishers-for-all-package-repositories.html) (May 2025; support list "as of December 2025"), [Build Provenance for All Package Registries](https://repos.openssf.org/build-provenance-for-all-package-registries.html) (July 2023 [STALE])

- **Principles v0.1:** level 3 includes trusted publishing (short-lived OIDC credentials), provenance support, a public transparency log of repository events; level 2 includes "Published versions cannot be replaced. Deleted name/version pairs cannot be reused." The document does **not** prescribe publisher-identity binding, client signature verification, or key and metadata signing/rotation (explicit gaps).
- **Trusted Publishers:** the maintainer pre-configures a trust policy on the repository, attached to a package or namespace; the repository verifies the OIDC token and matches provider claims (GitHub examples: `sub`, `repository`, `repository_owner`, `repository_owner_id`, `workflow_ref`); generic claims alone are insufficient. **Pending publishers** convert to normal ones on first upload. Guidance: "prefer immutable identifiers (such as GitHub owner IDs) over mutable names", resolve names to IDs at configuration time, beware resurrection after rename. The page does not discuss revocation or client-side behavior.
- **Build Provenance:** the registry verifies the Sigstore bundle, compares the source URI with the previous version, blocks silent provenance drop, records a publish attestation in Rekor; CLI verifies "the same way the registry does". Binds to workload identity, not a human.
- Reading (inference): the WG describes the index-holds-the-binding model and nothing about how a client learns the binding safely. That gap is exactly where the index-host-compromise threat sits.

### F15. Lighter alternatives: signed repository metadata, transparency logs, policy engines

Sources: [APT sources.list(5)](https://manpages.debian.org/trixie/apt/sources.list.5.en.html), [Go modules reference: authenticating modules](https://go.dev/ref/mod#authenticating), [Go sumdb design](https://go.googlesource.com/proposal/+/master/design/25530-sumdb.md) (2019 [STALE]), [Russ Cox, Transparent Logs for Skeptical Clients](https://research.swtch.com/tlog) (2019-03-01 [STALE]), [Sigsum docs](https://www.sigsum.org/docs/), [Sigsum named policies](https://www.glasklarteknik.se/post/named-policies-for-sigsum/) (2026-02-04), [Notation trust store and trust policy](https://github.com/notaryproject/specifications/blob/main/specs/trust-store-trust-policy.md) (undated)

- **APT (repository-signed index, one key, expiry):** `Signed-By` restricts a source to a keyring or fingerprints (operator-managed keyrings in `/etc/apt/keyrings`, package-managed in `/usr/share/keyrings`); `Check-Valid-Until` defends replay (default yes); `Valid-Until-Min/Max` stretch or cap validity, Min being "preferable to disabling the check" for seldom-updated local mirrors; `Trusted` overrides. No chain for key rotation; keyrings arrive out of band.
- **Go checksum database:** transparency log of `go.sum` lines (Trillian), signed tree head, inclusion and consistency proofs; `go.sum` plus cached tiles allow builds without contacting it; `GOSUMDB` accepts another database only with an explicit public key; `GONOSUMDB`/`GOPRIVATE` skip it per prefix; `GOSUMDB=off` gives up verification for all modules. It "makes forks and rewrites detectable, not impossible"; it does not address a malicious first publish; verifier-key rotation is a new path such as `sum.golang.org/v2`. No freshness or expiry concept.
- **Sigsum:** key-usage transparency; a policy lists log keys, witness keys and a quorum (for example 3 of 5); proof verification is offline; tools ship immutable named policies (first production one `sigsum-generic-2025-1`), others are installed files. Proof bundle format and trust-policy format are still listed as work in progress on the docs index.
- **Notation (OCI):** one trust policy per registry scope (exact match, then global fallback; overlapping identities invalidate the policy), levels strict/permissive/audit/skip, trust stores per type, `verifyTimestamp` with `afterCertExpiry`, revocation via OCSP/CRL. Distribution of stores and policies is "outside its scope".
- **Which threats each drops relative to full TUF (analysis):**
  - Signed index with one key and `Valid-Until`: loses threshold protection, root chaining (anchor rotation out of band), delegation, and mix-and-match protection unless a single signature covers the whole index.
  - Transparency logs: lose prevention, freeze protection, and revocation semantics; gain split-view detection and cheap offline proofs.
  - Policy engines (Notation): no freshness or distribution at all.

### F16. Lockfile-recorded identities and cached trust material

Sources: [PEP 751](https://peps.python.org/pep-0751/) (Final, resolved 2025-03-31 [STALE, marginal]), [Trail of Bits](https://blog.trailofbits.com/2024/11/14/attestations-a-new-generation-of-signatures-on-pypi/) [STALE], [sigstore-python tuf.py](https://github.com/sigstore/sigstore-python/blob/main/sigstore/_internal/tuf.py), [gh offline docs](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline)

- `pylock.toml` has `[[packages.attestation-identities]]` (`kind`, plus publisher keys such as `repository`, `workflow`, `environment`); tools SHOULD record identities when available. The PEP does not define installer verification or TOFU.
- Trail of Bits lists as future work: a pip verification plugin architecture, "trust-on-first-use for signing identities, so installs can halt when an identity changes", and "storing expected identities in standardized lockfiles, following PEP 751".
- sigstore-python offline mode never builds a TUF `Updater`; it logs "Using unverified trusted root from cache" and reads the targets cache. It copies an embedded `trusted_root.json` into the cache if absent.
- Relevance: the discussion rejected "recording the verifying signer in `ocx.lock`" as bespoke. This is independent prior art for the shape (an identity expectation recorded in a lockfile), published by the Python packaging ecosystem and Trail of Bits. Whether it qualifies is the ADR's call.

### F17. Time-bounded acceptance via signed time

Sources: [Sigstore TrustedRoot proto](https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_trustroot.proto), [TAP 18](https://github.com/theupdateframework/taps/blob/master/tap18.md) [STALE], [Notation spec](https://github.com/notaryproject/specifications/blob/main/specs/trust-store-trust-policy.md)

- TrustedRoot: "The embedded transparency logs, CT logs, CAs and TSAs MUST include any previously used instance -- otherwise signatures made in the past cannot be verified." Clients "MUST accept instances that overlaps in time" and lists whose last instance has a `valid_for` in the future, to survive planned rotations. Time ranges are inclusive; timestamps are checked against both the range and the whole chain.
- TAP 18 requires the Rekor SET to fall inside the certificate validity; Notation has `verifyTimestamp` (`always` or `afterCertExpiry`) and `authenticTimestamp`.
- Scope of this prior art (inference): service keys (logs, CAs, TSAs) and X.509 chains. No surveyed adopter applies validity windows to a set of publisher identities stored in TUF metadata. The primitive is established; the application to publisher sets is new.

### F18. Operational incidents (client compatibility and expiry)

Sources: [Sigstore TUF root update](https://blog.sigstore.dev/tuf-root-update/) (2024-03-14 [STALE]), [Docker retirement](https://www.docker.com/blog/retiring-docker-content-trust/), [Fulcio README via search result](https://www.github.com/yob/fulcio) (second hand)

- Sigstore's March 2024 root "does not contain any functional changes" but moves to the latest TUF spec version; cosign before 2.2.0 (2023-08-31) and 1.x could not load it, and "sigstore-rust's TUF client doesn't support the latest spec". Operating TUF means every spec-level change is a client-compatibility event.
- DCT: client-cached certificates that are never refreshed turned a scheduled expiry into mass pull failures for users with `DOCKER_CONTENT_TRUST=1`.

## Matrices

Mechanism columns (A-F). Ratings: **Yes** meets the requirement, **Partial** with the stated gap, **No**, **n/a**. Evidence lives in the cited findings.

| Code | Mechanism | Real adopters |
|---|---|---|
| A | Registry-style binding: signer data stored in the index, host trusted | npm/PyPI/crates.io trusted publishing (F14), PEP 740 (F6) |
| B | Repository-signed index, one online key or CI identity, expiry plus version counter | APT Release (F15), crates.io online index key inside RFC 3724 (F13) |
| C | TUF minimum model: offline root, online snapshot/timestamp, online delegated role under offline root | PEP 458 (spec only), tuf-on-ci/Sigstore, GitHub, RFC 3724 (F6, F10, F11, F13) |
| D | TUF maximum model: per-publisher offline keys | PEP 480 (Draft), DCT (retiring), Datadog (one org) (F6, F7, F9) |
| E | TUF with Fulcio identities (TAP 18) | go-tuf PR, securesystemslib, tuf-on-ci experimental (F3) |
| F | Transparency log with offline proofs | Go sumdb, Sigsum (F15) |

### M1. Requirement x mechanism

| Requirement | A | B | C | D | E | F |
|---|---|---|---|---|---|---|
| 1. Self-service onboarding, no client release | **Yes**: PR or pending-publisher flow (F14); client just reads the index | **Yes**: new file in the signed index | **Yes** for files under an existing online delegation; a new delegated role is a root-quorum event (F10) | **No/Partial**: each publisher key is a delegator metadata change by offline holders; PEP 480 itself says one-time manual work (F6, F7) | **Partial**: no key to manage, but each delegation still changes delegator metadata; Draft, no Rust support (F3, F13) | **n/a**: a log records, it does not decide who may publish; Go trusts first use (F15) |
| 2. Rotation and revocation without breaking old releases, incl. compromise | **Partial**: index edit revokes; old releases need a validity or pinned-identity rule the mechanism does not give (F17) | **Partial**: anchor rotation out of band, no chain (F15); old-release rule not provided | **Yes** for the anchor (root chaining, F1) and for operator-key compromise (PEP 458 recovery, F6); old releases survive only while re-listed (F2); no per-key validity (F17) | **Partial**: TAP 8/20 give delegatee-side rotation and self-revocation (F2); DCT shows the failure: removal forces re-signing, lost keys need support (F7) | **Partial**: compromise needs delegator action (F3); ephemeral keys remove key-loss risk | **No**: detection only; no revocation semantics; verifier key change = new path (F15) |
| 3. Offline and air-gapped, incl. pinned lockfiles | **Yes** from the cached index; no freshness signal | **Yes** from cache; expiry forces a policy: APT `Valid-Until-Min/Max` (F15) | **Yes** from cached metadata plus embedded root; timestamp expiry (7 d in Sigstore) forces cache-use policy: TUF §5.1 fixed time, sigstore-python unverified cache, gh exported trust root with no revocation info (F1, F16, F11) | Same as C | **Yes**: TAP 18 verifies offline with the bundle, but needs the Fulcio root and Rekor keys out of band (F3) | **Yes** with a proof bundle or cached tiles and `go.sum`; first lookup is online (F15) |
| 4. Operator override (private mirror accepts only own signatures) | **Yes**: client config replaces the index and its signer data (OCX operator tier exists today) | **Yes**: APT `Signed-By`, Go `GOSUMDB` + key (F15) | **Yes**: swap TUF repository and root: cosign `--mirror/--root`, gh `--tuf-url/--tuf-root` (F11, F10); TUF-native form is TAP 4 mapping (F4) | Same as C | Same as C | **Yes**: `GOSUMDB`, Sigsum policy file or named policy (F15) |
| 5. Storage and mutation: where trust data lives, who changes it, freshness, rollback, freeze, mix-and-match | Lives in the index (git, PR-reviewed); none of rollback, freeze, mix-and-match covered against a host that lies | Lives in a signed file set; rollback and freeze need counter plus expiry; mix-and-match covered only if one signature spans the whole index | Full: version numbers, timestamp, snapshot hashes, consistent snapshots (F1); static files; git plus signing-event PRs (F10) | Full, plus publisher metadata that expires and must be re-signed (Datadog 1 year offline, 1 week online) (F9) | Full for TUF metadata; adds Fulcio and Rekor trust (F3) | Split-view and rewrite detection by consistency proofs; no freshness or freeze protection (F15) |

### M2. Threat x mechanism

Cell = what the mechanism does to the attack. "Prevents", "Detects", "Bounded" (limits blast radius), "No".

| Threat | A | B | C | D | E | F |
|---|---|---|---|---|---|---|
| T1. Index host, CDN or git compromise (attacker edits or replaces index files, signer lists, tag-to-digest maps) | **No**: host is trusted (PEP 740 admits it, F6) | **Prevents** if the signing key or identity is not on the compromised host; **No** if it is | **Prevents** for host, CDN, mirror (tuf-on-ci claim F10, PEP 458 Table 4 F6); **No** for the repository operator's own online path (PEP 458: "not against a compromise of PyPI itself") | **Prevents** even for repository compromise, for claimed packages (PEP 480 goal, F6) | Same as D, keyless | **Detects** (split views, rewrites) after the fact (F15) |
| T2. Registry compromise (GHCR blob, tag or referrer replaced or deleted) | **Partial**: digest pin in index plus Sigstore signature; deletion causes fail-closed | Same as A plus integrity of the tag-to-digest map | Same as B; digest pins sit in TUF-verified targets | Same | Same | Not applicable beyond log consistency |
| T3. Publisher CI compromise (attacker runs the legitimate workflow or steals its OIDC token and signs a bad build) | **No**: identity is valid; narrowing claims helps (F14) | **No** | **No** | **Prevents** only where a human key signs each release (Datadog `tag` step with YubiKeys) and an in-toto layout checks the build (F9) | **No** | **Detects** (Rekor-style log shows what was signed; nesbitt reports Ultralytics, TanStack incidents as signing helping only forensics) |
| T4. Rollback or freeze of trust data (old signer list or stale index served) | **No** | **Partial**: needs expiry and monotonic counter (APT `Valid-Until`, F15) | **Prevents**: version monotonicity, timestamp expiry, fixed update time (F1) | **Prevents** | **Prevents** | **Detects** rollback via cached tree heads; **No** for freeze (F15) |
| T5. Publisher account takeover (attacker opens an index PR changing signers or owners) | **Partial**: human review is the only control (OCX CONTRIBUTING today) | Same: signed after the merge | **Partial**: offline-quorum role for signer files and online role for tag refreshes limits it (F10 design) | **Prevents**: publisher offline key required | **Partial** | **Detects** |
| T6. Compromise of the index bot or online signing key | n/a | **No**: key is the anchor | **Bounded**: key limited to its delegated paths; root revokes it and rotates timestamp/snapshot (spec §5.3 step 11, F1; PEP 458 recovery, F6) | **Bounded** to unclaimed content | **Bounded** | n/a |

### M3. Operational cost

| | A | B | C (tuf-on-ci shape) | D | E | F |
|---|---|---|---|---|---|---|
| Key ceremonies | none | one key generation; rotation out of band | Root and targets quorum with hardware keys: Sigstore 3 of 5, Rust RFC 5 of 9 (F10, F13); a root event roughly every 150 days at Sigstore settings (197 d expiry, 46 d window; version 16 since 2021) | Per-publisher key creation and backup; DCT "losing root key is unrecoverable" (F7); PEP 480 names key management a major obstacle (F6) | none for publishers | none; witness and log operators run theirs |
| Online keys | none | 1 (the anchor) | 1 KMS key for snapshot/timestamp, plus one per online delegation (Azure KMS for npm) (F10) | Shared online key for unclaimed content (PEP 458 shape) | as D | log signing key |
| Re-sign cadence | on change | per publish | Timestamp every <= 6 days (7 d expiry) automatically; targets/snapshot long-lived; root ~2-3 per year; third-party delegation ~every 151 days (182 d expiry, 31 d window) (F10) | Offline roles expire yearly (Datadog 1 year; PEP 458 1 year) (F9, F6) | as D | none per client |
| Client complexity | trivial | small (verify signature, expiry, counter) | TUF client: `tough` exists in the dependency graph; no succinct roles; spec bumps broke clients (F13, F18) | TUF client plus per-publisher key handling | TUF client plus unsupported keytype (F3) | proof verification; policy format still in flux (F15) |
| Infrastructure | none new | CI to sign and publish | GitHub Actions, Pages, a KMS key; no service to run (F10); RSTUF is the heavier service route (F12) | Notary-style service (DCT: server, signer, MySQL, mTLS) (F7) | as C | log, witnesses |
| Humans required | 1 | 1 | Threshold implies multiple key holders; a single owner gets no threshold benefit (inference) | Every publisher | none beyond C | witness operators |

### M4. Rust client libraries

| Library | Release or activity (2026-10-25) | Delegations | Hashed bins | Keytypes | Maturity statement |
|---|---|---|---|---|---|
| `tough` ([repo](https://github.com/awslabs/tough)) | `tough-v0.24.0` 2026-07-10 | `terminating`, `paths`, `path_hash_prefixes` | no `succinct_roles` | RSA, Ed25519, ECDSA | library plus `tuftool`; already in OCX's lockfile at 0.22.0 |
| `tuf` / rust-tuf ([repo](https://github.com/theupdateframework/rust-tuf)) | `0.3.0-beta15`, commits to 2026-08-24 | `terminating` | none found | Ed25519 in the crypto module | README: "Beta Software... API is unstable" |
| python-tuf ([repo](https://github.com/theupdateframework/python-tuf)) | v7.0.1, 2026-09-01 | full | `SuccinctRoles` flagged non-spec | via securesystemslib incl. experimental `sigstore-oidc` | reference implementation |
| go-tuf ([repo](https://github.com/theupdateframework/go-tuf)) | v2.4.2, 2026-05-19 | full | n/a here | n/a here | named by the Sigstore guide as a way to build a private TUF repository ([custom components](https://docs.sigstore.dev/cosign/system_config/custom_components/)); TAP 18 reference PR targets it |

## Sources

Ages are relative to 2026-10-25; `STALE` = older than about 18 months (before 2025-04-25).

| ID | Source | URL | Date | Flag |
|---|---|---|---|---|
| S1 | TUF specification 1.0.37 | https://theupdateframework.github.io/specification/latest/ | 2026-10-08 | fresh |
| S2 | TAP index | https://github.com/theupdateframework/taps | live | fresh |
| S3 | TAP 3 multi-role delegations | https://github.com/theupdateframework/taps/blob/master/tap3.md | 2019-02-12 | STALE |
| S4 | TAP 4 multiple repository consensus | https://github.com/theupdateframework/taps/blob/master/tap4.md | 2020-06-24 | STALE |
| S5 | TAP 8 key rotation | https://github.com/theupdateframework/taps/blob/master/tap8.md | 2024-03-26 | STALE |
| S6 | TAP 15 succinct hashed bins | https://github.com/theupdateframework/taps/blob/master/tap15.md | 2022-06-09 | STALE |
| S7 | TAP 18 Fulcio identities | https://github.com/theupdateframework/taps/blob/master/tap18.md | 2023-04-06 | STALE |
| S8 | TAP 19 content-addressable systems | https://github.com/theupdateframework/taps/blob/master/tap19.md | 2023-04-25 | STALE |
| S9 | TAP 20 self-revocation | https://github.com/theupdateframework/taps/blob/master/tap20.md | 2024-01-29 | STALE |
| S10 | PEP 458 | https://peps.python.org/pep-0458/ | modified 2025-02-01 | STALE |
| S11 | PEP 480 | https://peps.python.org/pep-0480/ | page modified 2026-10-23; body 2014-2020 | STALE body |
| S12 | PEP 740 | https://peps.python.org/pep-0740/ | resolved 2024-07-17 | STALE |
| S13 | PEP 751 | https://peps.python.org/pep-0751/ | resolved 2025-03-31 | STALE (marginal) |
| S14 | PEP 458 status thread | https://discuss.python.org/t/pep-458-current-status-and-next-steps-feedback-requested/17211 | 2022-2023 | STALE |
| S15 | Warehouse issue 5247 | https://github.com/pypa/warehouse/issues/5247 | closed 2022 | STALE |
| S16 | EuroPython 2024 abstract | https://programme.europython.eu/europython-2024/talk/GGWUNA | 2024 | STALE |
| S17 | Trail of Bits, PyPI attestations | https://blog.trailofbits.com/2024/11/14/attestations-a-new-generation-of-signatures-on-pypi/ | 2024-11-14 | STALE |
| S18 | Sigstore, PyPI attestations GA | https://blog.sigstore.dev/pypi-attestations-ga/ | 2024-11-14 | STALE |
| S19 | SigstoreCon 2024, Woodruff abstract | https://sigstoreconday2024.sched.com/event/1n4pO/the-next-5-years-of-supply-chain-security-on-pypi-william-woodruff-trail-of-bits | 2024-11 | STALE |
| S20 | SigstoreCon 2024, Kukkonen abstract | https://sigstoreconday2024.sched.com/event/1n4pX/rewriting-root-signing-a-deep-dive-into-sigstore-trust-root-delivery-jussi-kukkonen-google | 2024-11-12 | STALE |
| S21 | Docker, Retiring DCT | https://www.docker.com/blog/retiring-docker-content-trust/ | 2025-07-29 | fresh |
| S22 | Docker, retirement and migration guidance | https://www.docker.com/blog/docker-content-trust-retirement-and-migration-guidance/ | 2026-06-16 | fresh |
| S23 | InfoQ, DCT retired | https://www.infoq.com/news/2025/08/docker-content-trust-retired | 2025-08 | fresh |
| S24 | Docker DCT overview and delegations | https://docs.docker.com/engine/security/trust/ , https://docs.docker.com/engine/security/trust/trust_delegation/ | undated | current |
| S25 | Uptane Standard 2.1.0 | https://uptane.org/docs/2.1.0/standard/uptane-standard | undated | current |
| S26 | Datadog, secure publication of integrations | https://www.datadoghq.com/blog/engineering/secure-publication-of-datadog-agent-integrations-with-tuf-and-in-toto | 2019-06-03 | STALE |
| S27 | in-toto ITE index and ITE-3 | https://github.com/in-toto/ite , https://github.com/in-toto/ite/blob/master/ITE/3/README.adoc | 2019-07-19 | STALE |
| S28 | tuf-on-ci repo and docs | https://github.com/theupdateframework/tuf-on-ci | live 2026 | fresh |
| S29 | Sigstore root-signing repo and live metadata | https://github.com/sigstore/root-signing , https://raw.githubusercontent.com/sigstore/root-signing/main/metadata/root.json | fetched 2026-10-25 | fresh |
| S30 | Sigstore security model | https://docs.sigstore.dev/about/security/ | undated | current |
| S31 | Sigstore, TUF root update | https://blog.sigstore.dev/tuf-root-update/ | 2024-03-14 | STALE |
| S32 | GitHub, Artifact Attestations beta | https://github.blog/security/supply-chain-security/introducing-artifact-attestations-now-in-public-beta/ | 2024-05-02 | STALE |
| S33 | GitHub docs, offline attestation verification | https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline | undated | current |
| S34 | `gh attestation trusted-root` manual | https://cli.github.com/manual/gh_attestation_trusted-root | undated | current |
| S35 | RSTUF 1.0.0 and docs | https://repository-service-tuf.readthedocs.io/en/latest/blog/posts/rstuf-v1.0.0-is-out.html | 2025-08-29 | fresh |
| S36 | Rust RFC 3724 (PR and rendered text) | https://github.com/rust-lang/rfcs/pull/3724 , https://github.com/walterhpearce/rfcs/blob/tuf/text/3724-theupdateframework.md | opened 2024-10-31, updated 2026-10-02 | STALE origin, live |
| S37 | Rust 2026 mirroring goal | https://goals.rust-lang.org/2026/mirroring.html | 2026 | fresh |
| S38 | `tough` repo and changelog | https://github.com/awslabs/tough | 2026-07-10 | fresh |
| S39 | rust-tuf | https://github.com/theupdateframework/rust-tuf | 2026-08-24 | fresh |
| S40 | sigstore-rs Cargo.toml | https://github.com/sigstore/sigstore-rs/blob/main/Cargo.toml | 0.14.0 | fresh |
| S41 | securesystemslib SigstoreKey | https://github.com/secure-systems-lab/securesystemslib/blob/main/securesystemslib/signer/_sigstore_signer.py | live | fresh |
| S42 | python-tuf `SuccinctRoles` docs | https://theupdateframework.readthedocs.io/en/latest/api/tuf.api.metadata.supporting.html | live | fresh |
| S43 | sigstore-python `tuf.py` | https://github.com/sigstore/sigstore-python/blob/main/sigstore/_internal/tuf.py | live | fresh |
| S44 | Sigstore TrustedRoot proto | https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_trustroot.proto | live | fresh |
| S45 | Cosign custom components | https://docs.sigstore.dev/cosign/system_config/custom_components/ | undated | current |
| S46 | OpenSSF Principles for Package Repository Security | https://repos.openssf.org/principles-for-package-repository-security.html | v0.1, 2024-02 | STALE |
| S47 | OpenSSF Trusted Publishers for All | https://repos.openssf.org/trusted-publishers-for-all-package-repositories.html | 2025-05 | fresh |
| S48 | OpenSSF Build Provenance for All | https://repos.openssf.org/build-provenance-for-all-package-registries.html | 2023-07 | STALE |
| S49 | Go modules reference | https://go.dev/ref/mod#authenticating | current | fresh |
| S50 | Go sumdb design | https://go.googlesource.com/proposal/+/master/design/25530-sumdb.md | 2019 | STALE |
| S51 | Cox, Transparent Logs for Skeptical Clients | https://research.swtch.com/tlog | 2019-03-01 | STALE |
| S52 | Sigsum docs and named policies | https://www.sigsum.org/docs/ , https://www.glasklarteknik.se/post/named-policies-for-sigsum/ | 2026-02-04 | fresh |
| S53 | APT sources.list(5) | https://manpages.debian.org/trixie/apt/sources.list.5.en.html | current | fresh |
| S54 | Notation trust store and trust policy spec | https://github.com/notaryproject/specifications/blob/main/specs/trust-store-trust-policy.md | undated | current |
| S55 | Nesbitt, "Signing is for the bad days" (opinion) | https://nesbitt.io/2026/05/24/signing-is-for-the-bad-days | 2026-05-24 | fresh, secondary |
| S56 | Square, Securing RubyGems with TUF part 1 | https://developer.squareup.com/blog/securing-rubygems-with-tuf-part-1/ | 2013-12-06 | STALE, background only |
| S57 | OneUptime, private Fulcio root via TUF (third-party blog, low authority) | https://oneuptime.com/blog/post/2026-08-26-distribute-private-fulcio-trust-root-tuf/markdown | 2026-08-26 | fresh, secondary |
| S58 | Local: OCX dependency facts | `/home/mherwig/dev/ocx/crates/ocx_sign/Cargo.toml`, `/home/mherwig/dev/ocx/Cargo.lock` (`tough 0.22.0`) | 2026-10-25 | local |

**Not found or unverified** (so the ADR does not rely on them):
- A production PyPI deployment of PEP 458 (no source either way after the 2024 abstract).
- Any production system that distributes publisher identity sets through TUF.
- A `sigstore-oidc` implementation beyond securesystemslib and the go-tuf PR.
- Why PyPI chose PEP 740 over PEP 458/480 (no source states a reason).
- Whether `tuf-on-ci`'s online delegation scales to frequent bot-driven merges (it is documented as experimental with a PR-per-event flow).
- `tough` support for TAP 8/20 rotate files (not found in the schema listing; not exhaustively checked).

## Recommendation

**Verdict.** Use TUF for what the evidence shows it does well here: protect the index as a whole and give clients one rotatable anchor. Leave publisher trust to the Sigstore identity binding stored in the index, as the trusted-publisher ecosystems do. Do not give publishers TUF keys, and do not wait for TAP 18.

**Why this shape (three points).**
1. It is the only option in the survey that satisfies requirements 2 and 5 together without a client release. Root chaining rotates the anchor, snapshot/timestamp give rollback, freeze and mix-and-match protection, and the root can revoke a compromised online key (F1, F6). A pinned CI identity or bare signing key has no chain; Sigsum and APT ship rotation in a tool or package release (F15).
2. It is what the nearest real deployments run. Sigstore's repository runs 3-of-5 offline root and targets, one online KMS key for snapshot and timestamp (7-day timestamp), and one online-key delegation to a third party. Rust RFC 3724 proposes a single online index key under a quorum root and signs the index, not the artifacts (F10, F13).
3. The expensive alternatives failed in the field: per-publisher offline keys (PEP 480 Draft since 2014, DCT being shut down 2026-12-08) and a Draft keyless-in-TUF TAP (F3, F6, F7).

**Mapping onto the five requirements.**

| Requirement | Mechanism | Adopter evidence |
|---|---|---|
| 1. Onboarding | Signer binding (workflow identity, issuer, immutable repository and owner IDs) lives in the package's index file, added by PR; new package = new file under the online-signed role, no client change. Pending-publisher style for first publish. | OpenSSF Trusted Publishers (F14), npm/PyPI/crates.io; PEP 751 field shape (F16) |
| 2. Rotation, revocation, old releases | Anchor: TUF root chaining. Signer set: allow overlap (any-of) during rotation; on compromise either re-attest affected digests or apply a time cutoff from signed time. Both have prior art (DCT re-sign, F7; TrustedRoot `valid_for` and TAP 18 SET rule, F17). The ADR must pick; a cutoff avoids the 8-candidate referrer cap. | F2, F7, F17 |
| 3. Offline | Client caches last verified TUF metadata and the root chain; explicit staleness policy (fail-closed only when no cache exists, warn when timestamp is expired); `ocx.lock` keeps digests and, per prior art, may carry the expected identity. | F11, F16 (sigstore-python, gh, PEP 751) |
| 4. Operator override | Swap the anchor and repository (URL plus root) for a scope; the existing operator tier already replaces defaults. A private mirror runs its own TUF repository and signer data. TAP 4 mapping is the TUF-native form if two sources must be combined. | F4, F10, F11, F15 |
| 5. Storage and mutation | Static TUF metadata beside the index, produced by tuf-on-ci in a git repository, published to static hosting; changes land by PR; snapshot/timestamp online. | F10 |

**Concrete minimum viable design (tuf-on-ci shape, variant 2 of two).**
- *Variant 1 (RFC 3724 shape, smallest):* offline root; one online key signing targets, snapshot and timestamp; zero delegations. Protects against host, CDN and git compromise; the bot key is a full authority until root revokes it.
- *Variant 2 (Sigstore shape, recommended):* offline root and targets (hardware keys, threshold set to what the actual number of key holders supports); one KMS-held online key for snapshot and timestamp; one **online delegated role** for bot-written files (tag refreshes); signer and owner changes stay under the offline targets role. Expiry per Sigstore: timestamp 7 d (signing window 6 d), snapshot and targets long-lived, root about 200 d.
- *Design consequence:* TUF delegation is per file path, not per JSON field. The current index root mixes `repository` and `owners` (human-reviewed) with `tags` (bot-refreshed) in one file. Variant 2 therefore needs those two trust levels in separate files, or the whole file under the stricter role. This is a wire-format decision for the ADR (the index format is an interface per `CLAUDE.md`).
- *Client:* embedded initial `root.json`; `tough` (already a transitive dependency at 0.22.0 via the sigstore fork); delegations by `paths` only, no hashed bins at tens or hundreds of packages. Unknown signer-matcher kinds in the index must fail closed (TAP 18's compatibility note shows the opposite default is easy to get wrong).
- *CI flow:* the bot's refresh PRs would have to be `sign/*` branches using tuf-on-ci's online delegation, which is documented as experimental with one online signer per delegation; prove this on a staging repository first, as Sigstore does.

**What not to do.**
- Per-publisher TUF keys (PEP 480, DCT) and TAP 18 delegations: no Rust key type, Draft, unstable (F3, F6, F7, F13).
- PEP 458 hashed bins or RSTUF: sized for hundreds of thousands of files and a service stack (F6, F12).
- Transparency logs as the primary mechanism: detection without revocation or freeze protection (F15). Add Rekor-style witnessing later if split-view becomes a stated threat.
- Treating TUF as a publisher-CI-compromise control. It is not; only in-toto layouts address that, at Datadog-level cost (F9). Narrow identities (workflow ref, environment, immutable IDs) and rely on Sigstore detection.

**Risks and open decisions for the ADR.**
1. **Threshold vs the number of humans.** A 3-of-5 root assumes five key holders; a solo owner gets no threshold protection and should say so (inference). Hardware keys held by one person still defend against host compromise (F10).
2. **Format-compat events.** TUF spec bumps can force a client release (F18); `tough` lags the spec (no succinct roles, last Sigstore bump broke the Rust client). Pin the TUF spec version OCX publishes.
3. **Online delegation maturity.** Documented as experimental (F10); the fallback is Variant 1.
4. **Old-release rule.** The choice between overlap plus re-attestation and a signed-time cutoff determines whether the signer data needs a time dimension. Prior art exists for the primitive (F17) and for lockfile-recorded identities (F16); no adopter combines either with TUF-distributed publisher sets, so the ADR should state that it is the first.
5. **Timestamp vs offline-first.** A 7-day timestamp makes every fully offline machine stale within a week; the staleness policy (warn vs fail, configurable per scope) is a product decision, not a protocol one (F11, F16).
6. **Index schema ownership.** The split of `repository`/`owners` from `tags` (above) and any `signers` field are interface changes covered by `CLAUDE.md` stability rules, with the read path for published packages kept backward compatible.
