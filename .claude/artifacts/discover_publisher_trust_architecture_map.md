# Discover: publisher-signer trust — architecture map

## Metadata

- **Date:** 2026-10-25
- **Producer:** hex-architect xhigh Phase 1, run by the `architecture-explorer` worker (sonnet), read-only.
- **Dossier:** `.agents/discussions/release-signing-trust-default.md`
- **Confidence:** every statement is code-verified unless it is marked `[doc]`.

**Verdict.** The plumbing for distributing trust without an ocx release already exists: the managed config tier carries keyless `[[trust.policy]]` rows today. Four things are missing:

- a default policy;
- per-publisher trust data in the index;
- signature checking on the bot side;
- rotation and revocation semantics beyond ANY-of overlap and the Rekor integrated time.

## Part A — claim diff

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | No built-in trust policy exists. | Still true. | `crates/ocx_config/src/lib.rs:314` (accessor) and `:1479-1480` (test `trust_policies_empty_when_absent`) |
| 2 | Without a covering policy, install skips verification. | Still true. | `crates/ocx_package_manager/src/tasks/auto_verify.rs:129-131` logs and returns `Ok(())`. `--no-verify` is only relevant when a policy covers the package (`:135-142`). |
| 3 | Auto-verify attaches only when operator policies exist. | Still true. | `crates/ocx_cli/src/app/context.rs:398-417`. `project_policies` is "empty for OCI-tier install/pull" (`auto_verify.rs:34`). |
| 4 | The policy grammar is scope + SAN/issuer, resolved per tier. | Still true. | `crates/ocx_trust/src/lib.rs` — `TrustPolicy` `:368-409`, `KeylessMatcher` `:417-433`, `SignerSpec` `:451-466`, `IdentityRule` `:809-834`, `resolve` `:842-878`, `resolve_tiered` `:889-901`. Matching reads only the SAN and the issuer (`crates/ocx_sign/src/verify/identity.rs:56-90`). |
| 5 | The index root wire format tolerates unknown fields. | Client: still true. Server: closed. | Client `crates/ocx_index/src/wire.rs:52-67` has no `deny_unknown_fields`. Server `ocx-sh/index` `schema/root.schema.json:99` sets `additionalProperties:false`, and `tagEntry`, `owner`, `upstream` and `yanked` are closed too. |
| 6 | Signing never verifies against an expected identity. | Still true; line range drifted. | `crates/ocx_sign/src/sign/pipeline.rs:138-145` (`identity_from_pem` / `issuer_from_pem`). The read-back tests now sit at ~`:1131-1218`. No expected-identity check exists. |
| 7 | Only `sdkgen-publish.yml` signs with ocx in CI. | Still true. | `.github/workflows/sdkgen-publish.yml:209-256` (job `sign` holds `id-token: write` at `:221`). `docker-publish.yml:207` and `build-windows-shims.yml:245` use GitHub build-provenance attestations, which ocx cannot verify. `ocx/cli` is never signed with ocx. |
| 8 | Index governance is owner-gated, and `repository`/`owners` edits need human review. | Still true. | `ocx-sh/index` `CONTRIBUTING.md:28-58`, `site/src/docs/reference/governance-contracts.md:20,34` (G-05, G-19), `schema/root.schema.json:111-141`. Ownership key: numeric owner `id`. |
| 9 | Offline trust-root ladder exists, plus `config push` inlining and a digest-pin guard. | Still true. | Ladder: `adr_offline_verify_trust_cache.md:122-135`. Cache: `crates/ocx_sign/src/verify/trust_cache.rs:17-18` (24 h). Inlining: `crates/ocx_package_manager/src/managed_config/publish.rs:288`. Guard: `crates/ocx_config/src/loader.rs:320-381`. |
| 10 | `ocx.lock` carries no trust data. | Still true. | `lock_version = 3`, `declaration_hash`, and `[[tool]]` rows mapping platform → `sha256:` digest. |

## B1 — Module map

| Module | Key types | Relevance |
|---|---|---|
| `crates/ocx_trust/src/lib.rs` (ecosystem) | `TrustConfig`, `TrustPolicy`, `SignerSpec::{Keyless,Key,Unknown}`, `KeylessMatcher`, `IdentityRule`, `ScopeSpec`, `resolve`, `resolve_tiered`, `policies_from_ocx_toml` (`:912`), `lock_as_system` (`:42`) | The only trust grammar. Any new data source must compile into `TrustPolicy` values. |
| `crates/ocx_sign/src/verify/` (ecosystem) | `VerifyPipeline`, `VerifyContext` (`pipeline.rs:140-170`), `VerifyResult` (`:187`), `resolve_trust_root`, `TrustRootCache`, `SigningInstant` | Cryptographic verification; policy-agnostic apart from `matching_policies`. |
| `crates/ocx_config/src/` (ecosystem) | `Config.trust` (`lib.rs:139`), `Config::merge` (`:253-269`), `ManagedConfig` / `ManagedConfigSnapshot` (`managed.rs`), `fold_managed_tier` (`loader.rs:198`), `guard_managed_sigstore_trust` (`:320`) | Tier merge and the managed-payload guard. |
| `crates/ocx_package_manager/src/managed_config/publish.rs` (internal) | `ocx config push` inlining | Publishing on the operator side. |
| `crates/ocx_package_manager/src/tasks/{auto_verify,verify,managed_config}.rs` | `maybe_auto_verify` (`auto_verify.rs:119`), `update_managed_config`, `probe_managed_config_digest`, `check_managed_config_refresh` | The install hook and the managed-config refresh. |
| `crates/ocx_index/src/*` (ecosystem) | `IndexRoot`, `RootTag`, `IndexFormatConfig`, `ChainedIndex`, `OcxIndex`, `LocalIndex` | The index client. |
| `crates/ocx_cli/src/app/context.rs` | `Context::try_init` (`:398-417`), `build_auto_verify` (`:930`) | The seam where verification is wired in. |
| `ocx-sh/index` (`/home/mherwig/dev/index`) | `schema/root.schema.json`, `.github/index-policy.json`, `render-deploy.yml` | The ocx.sh index: a static site on Cloudflare Pages. |
| `ocx-sh/indexbot` (`/home/mherwig/dev/ocx-indexbot`) | `core/{verify_claims,validate_entry,policy,render,regenerate}.py` | Writes the roots. Verifies **no** signatures. |
| `external/sigstore-rs` | `trust/sigstore/mod.rs`, `cosign/*` | Fork based on v0.14.0. |

## B2 — The verify path end to end

```
config.toml tiers (system/user/$OCX_HOME/managed snapshot) → ocx_config::Config.trust
ocx.toml [[trust.policy]] (project)                        → policies_from_ocx_toml
Context::try_init (context.rs:398-417) — AutoVerify only if operator policies are non-empty
PackageManager::setup_impl → maybe_auto_verify (auto_verify.rs:119)
   resolve_tiered(operator, project, target) (:125-127); empty → log, install unverified (:129-131)
   --no-verify / OCX_NO_VERIFY (:135-142); lazy resolve_trust_root, six rungs (:150-179)
verify_one → VerifyPipeline::run (ocx_sign/verify/pipeline.rs)
   registry: subject digest via the index chain (:791-827), referrers (:830)
   bundle: Fulcio chain, Rekor SET + inclusion proof (:1617-1637), integratedTime within the certificate window (:1383-1387)
   identity: SAN + issuer against matching_policies (identity.rs:56-90)
```

**Network.**

- The registry is always read, even with `--offline`.
- Verification makes no Rekor lookup. The bundle carries the SET and the inclusion proof (`pipeline.rs:891`).
- The Rekor key is fetched once, and only online when the trust root lacks it (`:162-173`, `:1706`).
- TUF is used only by `TrustRoot::load_embedded` (`trust_root.rs:73-84`, `tuf-repo-cdn.sigstore.dev`), on rung 6 of the ladder, and never offline.
- Offline mode needs a pinned Rekor key; without one it fails with `OfflineNoPinnedRekorKey` (`trust_resolve.rs:86-97`).

**Time semantics.**

- `SigningInstant` has a single variant, `TransparencyLog(i64)` (`signing_instant.rs:14-26`). A clock-reading or `notBefore`-derived variant is forbidden, and a source-scan test enforces that.
- `signed_at` is the Rekor integrated time. A key-mode signature with no transparency-log entry has `signed_at = None` (`pipeline.rs:1421-1437`).
- TSA timestamps are not read.
- **No policy field consumes the signing time.** No signer carries a not_before or not_after bound, and there is no revocation list.

**Fulcio extensions.**

- Only the SAN and the issuer OID `1.3.6.1.4.1.57264.1.8` are read (`identity.rs:18-46`).
- sigstore-rs defines `.1.1`–`.1.6` and parses the legacy workflow extensions (`src/cosign/constants.rs:19-29`, `signature_layers.rs:1049-1190`), but has no matcher for `.1.9` and above.
- Matching a repository ID or owner ID would need new OID reads in `identity.rs` and new `KeylessMatcher` fields.

**The managed config tier — an existing channel that needs no ocx release.**

- **Publishing.** `ocx config push` publishes `config.toml` as an OCX package. A path-form `trusted_root` is inlined as `trusted_root_json` (`publish.rs:288`). The payload cap is `MAX_MANAGED_CONFIG_BYTES` = 64 KiB (`adr_managed_config_tier.md:332-334`).
- **Pinning.** The consumer seed `[managed] source` may be digest-pinned. `snapshot_matches_source` (`managed.rs:102-105`) checks the snapshot against the pin.
- **The unpinned-payload guard** `guard_managed_sigstore_trust` (`loader.rs:320-381`):
  - It strips path-form `extra_ca_certs`, path-form `trusted_root`, and signer `key` paths. A stripped signer becomes `SignerSpec::Unknown`, which accepts nobody.
  - It strips `trusted_root_json`, `fulcio_url` and `rekor_url` only when the source carries no digest.
  - It keeps keyless `[[trust.policy]]` rows, `key_pem` signers and `extra_ca_certs_pem`, pinned or not.
  - **So an unpinned managed tag already distributes signer identities to a whole fleet.**
- `system_locked` is `serde(skip)`, so a payload cannot claim the system lock.
- **Refresh.** The snapshot lives at `state/managed-config/snapshot.json`. A background tick (`app/managed_config_check.rs`, disabled by `OCX_NO_CONFIG_REFRESH`) and `ocx config update [--check]` refresh it. A refresh failure never blocks a command.
- `required = true` (the default) with a missing or unparseable snapshot exits 78 (`managed.rs:161`, `:278`).

**The index client.**

- **No index document is signed.** Integrity comes from TLS plus content addressing (`[doc]` `subsystem-oci.md`):
  - a root is checked against its entry in `c/index.json`;
  - a dispatch object `o/<algo>/<hex>.json` is checked against the digest in its file name;
  - the physical pointer is bounded by the SSRF floor and `trusted_hosts`.
- `adr_servable_index_snapshot.md:230-236`: the digest check is "not a trust anchor"; "The index path's real trust boundary is the transport".
- **Local copy.** `$OCX_HOME/index/<source>/` is authored, merge-only, never pruned of a pin, and outside garbage collection. It pins tag → digest and the logical → physical routing. It is moved only by `ocx index update` and `ocx index sync`.
- **Correction to the memory note.** `ocx.lock` is index-free and stores platform leaf digests (`adr_platform_model_unification.md` D3).
- **Fetching.** `ChainedIndex` checks the local copy first, then exactly one remote per namespace, chosen by `[registries."<ns>"] index` and never probed. `config.json` carries `format_version`, gated by `SUPPORTED_FORMAT_VERSION = 1` (`wire.rs:14,23`).

**ocx-sh/index and indexbot.**

- **Deployment.** A static site on Cloudflare Pages, deployed by `render-deploy.yml` on governance dispatch, on push to main, or by a sparse `*/15` cron (median 75 min, worst case 222 min).
- **What the bot checks.**
  - It re-derives the claimed tag digests and CAS objects from the registry (`verify_claims.py`).
  - The registry host must be on the allowlist `.github/index-policy.json` (G-03).
  - Ownership is decided by the PR author's `github_id` appearing in `owners[]` (G-19).
- **The bot verifies no signatures.** `announce-a-package.md:259-261` says signing and attestation are "tracked separately, not part of this flow yet" ([ocx-sh/ocx#199](https://github.com/ocx-sh/ocx/issues/199), [ocx-sh/ocx#203](https://github.com/ocx-sh/ocx/pull/203)).
- `architecture.md:104-107` notes that a signature could target the image-index digest "without changing the graph". Non-platform descriptors (attestation, SBOM, signature) may ride in the same image index (`wire-format.md:197`, `entry-schema.md:155,164`).
- **Fields that bear on trust.**
  - `owners[]` holds numeric forge IDs.
  - `source` is bot-derived from the `org.opencontainers.image.source` annotation. That annotation is publisher-controlled and unverified.
  - `upstream`, `repository` and `status` are governed by humans.

## B3 — Patterns to follow

- **Fleet forward compatibility.** Configuration read across a fleet never uses `deny_unknown_fields`. An unknown value narrows (e.g. `SignerSpec::Unknown` accepts nobody); it never widens.
- **Exit codes.** Error variants declare them with `#[derive(Classify)]` and `#[exit(Code, slug, summary)]`.
- **CA-class trust is operator-only.** `[trust.sigstore]` comes from operator tiers only, replaces rather than appends, and locks per table.
- **Pin gating.** A payload field is honoured only when the seed is digest-pinned (`loader.rs:369`).
- **One merge seam.** `Config::merge` pools `trust.policy` and replaces `trust.sigstore` (`lib.rs:265-269`).
- **Source-scan guards.** "No X" invariants are enforced by scanning the source.
- **The index is authored, not mirrored.** Writes go through merge-only `RootScope::{Tags,Package,Routing}` writers.

## B4 — Reusable components

- `ocx_trust::{TrustPolicy, SignerSpec, KeylessMatcher, IdentityRule, ScopeSpec, resolve, resolve_tiered, TrustConfig::merge, lock_as_system}`. A published trust list can compile into `TrustPolicy` and reuse resolution, specificity and ANY-of rotation overlap unchanged.
- `ocx_sign::verify::{resolve_trust_root, TrustRootCache, matching_policies}`, and the 24 h state-file cache pattern.
- `ocx config push`, `update`, `probe_managed_config_digest`, `check_managed_config_refresh` and the background tick — publish, pin, fetch and refresh, with no ocx release.
- `ManagedConfigSnapshot` and `snapshot_matches_source`.
- `ChainedIndex` / `OcxIndex` / `LocalIndex` and `IndexRoot`.
- `maybe_auto_verify`, the single place install-time verification is enforced.
- Precedents on the server side: `index-policy.json` (G-03) and `maintainers.yml` (G-20), lists owned by the deployment and changed only by reviewed PRs.
- Test fixtures: `test/sigstore/trusted_root.json` and the self-hostable Sigstore stack.

## B5 — Existing channels for trust data that need no ocx release

| Channel | Carries today | Pin / integrity | Gap |
|---|---|---|---|
| `[managed]` config package | `[[trust.policy]]`, `[trust.sigstore]`, `extra_ca_certs_pem` | A digest-pinned seed is optional; unpinned still carries policies. | Per organisation, not global; needs an operator to run the seed. |
| `$OCX_HOME/sigstore/trusted-root.json` | Fulcio, CT and Rekor roots | Local file | Local only. |
| `[trust.sigstore] trusted_root(_json)` | Same as above | Operator tiers only | Replaces rather than merges. |
| Project `ocx.toml` `[[trust.policy]]` | Policies | Adds only where no operator policy matches. | Not used for OCI-tier install and pull. |
| Index wire (`config.json`, roots, `c/index.json`) | Nothing trust-related | TLS plus content addressing | The server schema is closed; carrying trust data needs bot, schema and render changes. |
| OCI referrers | Signature bundles | Verified against the policy | Carries signatures only, not a registry of signers. |

## B6 — Conventions

- **Errors.** thiserror plus `Classify`. A transient transparency-log failure (75) stays distinct from a policy refusal (81, 82, 78, 64).
- **Crate placement.** Grammar in `ocx_trust`, cryptography in `ocx_sign`, snapshot handling in `ocx_config` plus `ocx_package_manager`, wire format in `ocx_index`.
  - All are ecosystem crates, so ocx-mirror is upgraded in the same series (`task satellite:verify`).
- **Wire changes are interface decisions.** A gated machine document additionally needs a contract-ledger entry and a version bump.
- **Tests.**
  - Unit tests live inline; acceptance tests go under `test/tests/` and run against the Sigstore stack.
  - Every new guard needs a mutation proof: red, then green.
  - A new CLI verb needs a smoke marker and a `SUITE_FLOOR` bump.
- **Environment variables** are declared in `ocx_env/src/vars.rs` and documented in `reference/environment.md`.

## B7 — Prior decisions a new design must not violate

From `adr_trust_policy.md` (Proposed, MAX tier, one-way door, [ocx-sh/ocx#98](https://github.com/ocx-sh/ocx/issues/98)) unless noted:

- **D3 (`:264-293`).** "If **any** operator policy matches the target, only operator policies are evaluated and the project `ocx.toml` is **ignored for that package**." A project can "**never override or weaken** an operator pin". Reversing this needs an explicit owner decision (`:467-468`). **Any new tier, including one published through the index, sits below operator authority.**
- **D4 (`:295-318`).** The longest literal prefix wins. ANY-of among equals is the rotation-overlap mechanism.
- **D6 (`:391-418`).** An exact identity is byte-equal, a regex is anchored to the full string, and the issuer is always exact.
- **Amendment of 2026-08-02 (`:35-74`).** A system-locked pin fixes the specificity level. Lower tiers may join the ANY-of set but cannot outbid it. The lock bounds substitution and denial, "NOT admission": "Write access to any of those three surfaces is therefore trust-equivalent to the system tier for **adding** an accepted signer." Cross-tier ANY-of is the "fleet key-rotation capability". → **A signer list distributed through the index is an admission surface, so it needs an explicit threat decision.**
- **Invariants.**
  - An empty `signers` list is an error.
  - An unknown `kind` narrows.
  - `system_locked` cannot be set from a file.
  - `[trust.sigstore]` is operator-only: "a repository that could name its own Fulcio CA would verify its own signatures" (`ocx_trust/src/lib.rs:32`).
- **S1-H (`adr_oci_referrers_signing_v1.md:487-504`).** There is no escape hatch, and Rekor availability is a hard dependency; `--insecure-ignore-tlog` was rejected.
- **Servable-snapshot ADR.** DR2 (`:125`): "No check may be selected by which caller asked. Trust rules are uniform over bytes." Signing is out of its scope (`:115`). Decision A (`:138`): an absent `config.json` means version 1, and a major-version mismatch is a hard failure.
- **Announce ADR** (`adr_announce_publisher_surface.md:37-38`). Ownership is `owners[].github_id`, and verification re-derives everything from the registry: "nothing claimed is trusted". **A signer field set by the publisher breaks that premise unless it is re-derived or cross-checked.**
- **Index governance (G-05, G-19).** A change to a signer list belongs with the human-review keys.
- **Breaks.** Already-published packages must keep resolving, so a new signature requirement may gate only new verifications. Today, validity is bounded only by the certificate window and the Rekor integrated time, and removing a policy is never retroactive.

## Not covered

- The `ocx-mirror` repository was not explored.
- Index `governance.yml` and `validate.yml` were not opened; the G-contracts come from the reference doc.
- Read only partially: `adr_announce_publisher_surface.md`, `adr_servable_index_snapshot.md`, `adr_index_claim_command.md`, `design_spec_servable_index_snapshot.md`, `research_index_wire_versioning_trust.md`.
- Not read: ETag and cache internals in `oci_index/cache.rs` and `chained_index.rs`, the `ocx package verify` and `ocx package sign` commands beyond their call sites, and the `ocx index sync` write path.
