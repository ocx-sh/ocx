# ADR: Publisher trust distribution — index-declared `publishers`, checked by the client

## Metadata

**Status:** Accepted (goal-loop authority, 2026-11-13; the owner may revisit)
**Date:** 2026-10-25 (drafted) · 2026-11-13 (Decision Outcome replaced for slice 1)
**Deciders:** Michael Herwig (owner); architect (Opus)
**Beads Issue:** N/A — related [ocx-sh/ocx#199](https://github.com/ocx-sh/ocx/issues/199), [ocx-sh/ocx#102](https://github.com/ocx-sh/ocx/issues/102), [ocx-sh/ocx#392](https://github.com/ocx-sh/ocx/issues/392), [ocx-sh/ocx#403](https://github.com/ocx-sh/ocx/issues/403), [ocx-sh/ocx-mirror#7](https://github.com/ocx-sh/ocx-mirror/issues/7)
**Source discussion:** `.agents/discussions/release-signing-trust-default-2026-11-12.md` (ratified 2026-11-13). It supersedes the 2026-10-25 Decision Outcome of this ADR where they conflict; this revision states that design.
**Related PRD:** `prd_release_signing_trust_default.md`
**Tech Strategy Alignment:**
- [x] Rust 2024 / Tokio; GitHub Actions + OIDC; no new crate (the 2026-10-25 `tough` dependency is dropped with TUF)
- [x] Secrets: none on the client; keyless signing needs none either
**Domain Tags:** security | integration | api
**Tier:** xhigh · **One-way door:** high (the `publishers` wire shape and the `claims` vocabulary ship in every client) · **Blast radius:** external contract (index wire format, config grammar, install exit codes)
**Supersedes:** the 2026-10-25 Decision Outcome of this ADR (option C1, TUF over the index)
**Amends:** `adr_trust_policy.md`, `adr_sbom_attestations.md`, `adr_offline_verify_trust_cache.md`, `adr_oci_referrers_signing_v1.md` (see [ADR amendments](#adr-amendments))
**Superseded By:** —

Citation keys: **[ECO]** `research_publisher_trust_package_ecosystems.md` · **[FW]** `research_publisher_trust_frameworks.md` · **[OCI]** `research_publisher_trust_oci_policy.md` · **[GAP]** `research_publisher_trust_gapcheck.md` · **[SSI]** `research_stable_signer_identity.md` · **[OID]** `research_fulcio_claim_oids.md` (all `.claude/artifacts/`).

## Context

The signing and verification machinery exists (`crates/ocx_sign`, `crates/ocx_trust`). No own release is signed, and verification on install runs only for a package an operator `[[trust.policy]]` covers (`crates/ocx_package_manager/src/tasks/auto_verify.rs`). Without one, install logs an INFO line and proceeds.

The 2026-10-25 draft made the index the trust point: the index bot checks publisher signatures at ingest, and clients trust whatever a TUF-signed index says. The owner rejected that premise on 2026-11-13. **The packages themselves must be validated by the client.** A mirror operator or a pass-through proxy has not validated every digest it serves, so it must not become the trust point. "The digest is enough" is not accepted. The product is decentralised: registries belong to different companies, each with its own public or private Sigstore instance, or a plain key.

The owner also dropped TUF metadata over the index. The index will be signed the way a package is, with an accepted-signer list in the same vocabulary (slice 4).

## Decision Drivers

- **Client-side validation.** A mirror or proxy never becomes the trust point (owner, 2026-11-13).
- **No client release for onboarding, rotation or a new signer kind.** An index edit is enough.
- **One signer vocabulary.** The index uses `[[trust.policy]]`'s signer shapes; GitLab and other forges are reached through generic claims, not a forge-specific kind.
- **Offline-first.** A committed local resolve stays network-free (Principle 2, `product-context.md`).
- **Fail closed on the unknown.** An entry a client does not understand matches nobody. It never matches everybody.
- **Prior art only.** Every mechanism has an adopter (table below).

## Industry Context & Research

| Mechanism | Adopter | Source |
|---|---|---|
| Publisher identity declared beside the package and checked on the consumer | aqua registry (`slsa_provenance`, `cosign` per package); Homebrew attestation checks; `gh attestation verify --repo` | [ECO] P4, P6 |
| Identity by immutable numeric repository and owner IDs | PyPI trusted publishing, sigstore-go `CertificateIdentity` extension matching, SLSA verifier | [GAP] G5, [SSI] |
| Generic certificate-extension match by OID | sigstore-go `certificate.Extensions`, cosign `--certificate-github-workflow-*`, policy-controller `subjectRegExp` + extensions | [OID], [GAP] G4 |
| An unknown entry kind disables only that entry | TUF delegations; npm provenance ignores unknown predicates | [FW] F18 |
| Several signed statements, the newest by log time wins | Sigstore bundle `integratedTime`; Rekor-ordered attestations in `gh attestation` | [OCI] |
| Per-repository verification mode `off` / `if-declared` / `required` | Go `GOSUMDB`/`GONOSUMCHECK`, pip `--require-hashes`, apt `Signed-By` | [FW] F15 |
| Operator policy ANDs over publisher declarations | policy-controller multiple `ClusterImagePolicy` (all must pass) | [OCI] |

## Considered Options

**A — operator policy only.** ocx ships a pointer to a default policy package; the index stays unsigned and declares nothing.
**B — index signs its catalog keylessly** with a compiled-in CI identity and bespoke `version`/`expires` rules; only the bot checks publishers.
**C1 — minimal TUF over the index** (2026-10-25 choice): offline root, one online key, the bot checks publishers, clients trust the TUF-signed index.
**C2 — TUF Variant 2:** C1 plus offline targets for declarations.
**E — package-style signer list (chosen).** The index package file declares `publishers` in the `[[trust.policy]]` signer vocabulary; the client verifies every fetched package against it. The catalog is later signed the same way, by a signer list per registry (slice 4). No TUF.
**D — unsigned declaration the client trusts blindly.** Eliminated by the 2026-10-25 owner ruling; E differs because the client checks the package signature itself and the index signature follows.

### Weighted matrix

Scores 1–5, weight × score. R6 is new (owner, 2026-11-13); the other rows keep their 2026-10-25 scores. Maximum 570.

| Criterion (weight) | A | B | C1 | C2 | E |
|---|---|---|---|---|---|
| R1 onboarding (12) | 3 | 5 | 5 | 3 | 5 — index PR only |
| R2 change safety (12) | 2 | 3 | 5 | 5 | 4 — add or drop entries; index-signer rotation by client release or operator config (slice 4) |
| R3 offline (10) | 4 | 4 | 4 | 4 | 4 — checks on fetch only |
| R4 override (8) | 5 | 5 | 4 | 4 | 5 — `signatures` per registry, `[[trust.policy]]` ANDs on top |
| R5 freshness (12) | 2 | 3 | 5 | 5 | 3 — `expires` arrives with slice 4 |
| Threats (14) | 3 | 4 | 4 | 5 | 4 — mirror and proxy are no trust point; index host partial until slice 4 |
| Solo cost (12) | 4 | 4 | 3 | 1 | 5 — no keys to guard |
| Client complexity (8) | 5 | 3 | 4 | 3 | 4 — reuses the verify pipeline; owns a claims matcher |
| Reversibility (6) | 4 | 2 | 4 | 3 | 4 — additive list, fail-closed growth |
| Fit (6) | 5 | 3 | 3 | 2 | 5 — same vocabulary as `[[trust.policy]]` |
| R6 packages validated by the client (14) | 2 | 1 | 1 | 1 | 5 |
| **Total** | **376 (66 %)** | **384 (67 %)** | **432 (76 %)** | **378 (66 %)** | **496 (87 %)** |

Flip points: E leads C1 by 64, of which R6 alone is 56. C1 wins only if R6 is weighted at 0, which the owner's ruling forbids. Without R6, E (426) and C1 (418) are within one score step on R5; slice 4's `expires` closes that gap.

### Rejected: TUF metadata over the index (C1, C2)

Dropped by the owner on 2026-11-13 for the package-style signer list. TUF made the index the trust point (contradicts R6), needed two offline root keys and an online key for a solo maintainer, and added `tough` plus a client datastore. Trust on first use for an index key is rejected as well: a CI runner is a first use on every run.

## Decision Outcome

**Chosen option: E.** Delivered in four slices (§ [Sequencing](#sequencing)). This section designs slice 1, the client, in full; the later slices are designed but not built.

### Decision log (slice 1)

Each row: question → evidence → decision.

| # | Question | Evidence | Decision |
|---|---|---|---|
| D-1 | Where does the index-side signer type live, and which crate edge? | `crate_map.toml:33`: `ocx_index` may not depend on `ocx_trust`; `ocx_trust` depends on `ocx_oci`, `ocx_util`, `ocx_env`; `ocx_package_manager` depends on both | `IndexRoot.publishers: Option<serde_json::Value>`, tolerant and uninterpreted in `ocx_index`. `ocx_trust` gains a fail-closed parser from that value. `ocx_package_manager` joins them. No new crate edge. |
| D-2 | Which fields does an index entry accept? Do operator signers gain `claims`? | `SignerSpec` (`ocx_trust/src/lib.rs:448-466`) tolerates unknown *fields* inside a known kind (tests ~:2060-2130); `KeyMatcher.key` reads a local path | Index entries parse strictly. `keyless`: `oidc_issuer` required, plus one or more of `identity` / `identity_regexp` / non-empty `claims`, and never both `identity` and `identity_regexp`. `key`: `key_pem` only. Unknown kind, unknown field, or `sigstore` (reserved) → the entry is *unrecognised* and matches nobody. A `key` field (a path) is *invalid* and is checked before the unknown-field rule; a `key_pem` of an unsupported key type → *unrecognised*, a malformed one → *invalid*; a malformed value, `null`, an empty or non-array `publishers` → *invalid*, matches nobody (`IndexRoot` carries every present value, `null` included). A remote `identity_regexp` compiles under a 64 KiB size limit. Operator `[[trust.policy]]` keyless signers gain the same `claims` map; their unknown-field tolerance is unchanged (known limitation, § Consequences). |
| D-3 | How do `claims` match? | [OID] decoding rules 1–9 | Key = a name from the table (`.1.8`–`.1.24`, minus `.1.7`) or a raw dotted OID under `1.3.6.1.4.1.57264.1.`. Canonical decimal arcs only (no leading zeros, u32, no sub-arcs). Raw value for N ≤ 6, strict minimal DER UTF8String for N ≥ 8 (and any N above the table); non-minimal DER or another tag is a non-match. Byte-exact string equality; every listed claim must match; an absent extension never matches; an extension present more than once is a non-match, for claims, `.1.8` and the SAN alike. `oidc_issuer` keeps comparing against `.1.8`; a certificate carrying only the deprecated `.1.1` is refused with a message naming `.1.8`. |
| D-4 | Check order and semantics | `maybe_auto_verify` (`auto_verify.rs:119`) checks opt-out after coverage; `AutoVerify` is built only with operator policies (`context.rs:467-486`) | `AutoVerify` is always built. Order: coverage (publishers lookup, registry mode, operator policy) → opt-out → publisher round → operator round. Each round is first-match ANY-of; both must pass, and both bind the same document for attestations. The Sigstore trust root resolves lazily, per candidate (D-13). |
| D-5 | What does "make no request" mean, and what of `ocx.lock` / digest pulls with no local root? | `setup_impl` (`pull.rs:252`) verifies before download; the store keys content by digest, so content installed unverified under another name (`pull_local`, `--no-verify`) would otherwise link unverified under a declared name; the registry client is live under `--offline`; digest pulls on an indexed namespace already commit the root on first sight (`RootScope::Routing`, `chained_index.rs:888`) | This feature adds **no index request**: `publishers` is read from the local roots (D-11), never fetched for the check. Every `setup_impl` leader verifies, as today. `find` / `find_or_install` of a package whose root declares `publishers` treats content as installed only when its `refs/origins/` holds a marker for that logical repository (written only on a genuine fetch, after the leader's check or an explicit `--no-verify`); otherwise it falls through to the pull, which verifies. Undeclared packages keep today's fast path. A lock or digest install on a home with no root for the package is *undeclared*: it installs as today with the INFO line, operator policy still applies, and `signatures = "required"` refuses 81 naming `ocx index update <pkg>` (S-016). Signature referrer fetches follow today's rules. |
| D-6 | The `signatures` setting | `RegistryConfig` (`ocx_config/src/registry.rs:13-52`), Some-wins merge, `system_locked`; IC-06 (snake_case values); unknown enum values fail a config parse | `[registries."<registry>"] signatures = "if_declared" \| "required" \| "off"`, default `if_declared`, keyed by the package's **logical** registry, exact match. `config.toml` tiers only (system, user, home, managed, explicit); never `ocx.toml`. An unknown value reads as `required` and logs one WARN (the variant is skipped in the JSON schema). `[registries]` keys are normalised like registry identifiers before the lookup. `off` skips the publisher round only; operator policy still applies. |
| D-7 | Exit codes and slugs (IC-22) | `adr_exit_code_taxonomy.md`; existing `identity_mismatch` 77, `no_signatures_found` 79, `signature_invalid` 65; the `ocx index update` hint belongs to the operations tier, not the ecosystem `ocx_sign` | Four new slugs, no new code, on `ocx_package_manager::tasks::signer_rounds::RoundsError` (`#[derive(Classify)]`; `ocx_sign::VerifyErrorKind` is unchanged; `ocx_trust::PublishersRefusal` stays unclassified): `publisher_mismatch` 77 (no listed publisher signed it; claims mismatch included), `publishers_unrecognized` 82 (no usable entry, at least one unrecognised: this build lacks a kind or field), `publishers_invalid` 65 (no usable entry, all invalid: fix the index data), `publishers_required` 81 (`signatures = "required"`, package undeclared: loosen the setting). |
| D-8 | Attestations | `package_sbom.rs:430-504` refuses several verified SBOMs (`multiple_attestations` 65); attestation signers come from `resolve_policies_lenient` (`package_sign_common.rs:293-316`); `signing_instant.rs` holds the log time | Without explicit signer flags, `ocx package verify` (both modes) and `ocx package sbom` resolve first, then read publishers, then verify with the same rounds as install. Explicit `--certificate-identity` / `--key` flags replace every round, as today. A document is eligible only when it verifies in every applicable round (matched by referrer digest). Among eligible attestations of one predicate type the newest by SET-verified transparency-log time wins; any candidate without a logged time, or a tie between different documents, refuses as today. With nothing covering (no flags, no policy, no publishers) the SBOM command keeps today's permissive mode. |
| D-9 | `--no-verify` / `OCX_NO_VERIFY` | `auto_verify.rs:135-142`; `test/tests/test_auto_verify.py:170` greps the warning | Skips both rounds and the `required` refusal. The warning text stays byte-identical, once per batch, and fires when any check would have run. |
| D-10 | Does `[[trust.policy]]` move into the registry table? | Discussion § Client: scopes span registries, carry `builder` and the system lock, and are the only signer source for a plain OCI address | It stays as is. Open question closed. |
| D-11 | Previous release, and the local-index merge of `publishers` | `IndexRoot` has no `deny_unknown_fields` (`wire.rs:52`); `merge_root` walks `serde_json::Value` (`local_index.rs:795`) and adopts package-level fields only on first sight and under `RootScope::Package`, overwrite-only | `merge_root` is unchanged: first sight carries `publishers`; `ocx index update <pkg>` overwrites it; a tag grow and a routing write never touch it. The list in force is computed at read time. Committed and a root fetched by this process both present → the element-wise intersection (JSON value equality); committed absent, fetched present → fetched (an index can only add a check to an undeclared package); fetched absent → committed (deleting the field never removes a check). An empty intersection refuses `publisher_mismatch` naming `ocx index update <pkg>`. The effective list never widens the committed one; an unreadable or unparsable committed root is an error, not "undeclared". The previous release ignores the field and installs unverified. |
| D-12 | End-to-end fixtures | Discussion § Sequencing; goal criterion 10 | `ocx-sh/e2e-package` publishes fixed cases with real GitHub-issued certificates; `ocx-sh/e2e-index` serves a static index on GitHub Pages; a `workflow_dispatch` workflow `client-e2e.yml` in `ocx-sh/e2e-index` (input `ocx_ref`) checks out this repository at that ref, builds the client and runs `test/e2e/` read-only against both. It lives there because GitHub dispatches a `workflow_dispatch` workflow only when its file is on the default branch, which this repository's branch is not. Out of `task verify`. Shape in system design C-019. |
| D-13 | Key candidates and transparency evidence | Requirement "a `key` publisher verifies without any Sigstore network"; the trust root resolves through the TUF ladder in `auto_verify.rs:150-153`, `package_verify.rs:156`, `package_sbom.rs:180-192`; a round may mix key and keyless entries | Transparency is decided per candidate: `Verify` or `VerifyIfRootAvailable`. A key candidate's log evidence is checked when a trust root is available with no network (override, home file, cache) and ignored otherwise (`signed_at = None`); `signed_at` comes from a verified SET only. The trust root resolves lazily on the first keyless candidate; its unavailability fails that candidate only and the round continues. Operator key-only policies keep log checks whenever the root is local. |
| D-14 | Status | Goal loop is unattended; owner ratified the source on 2026-11-13 | Accepted under goal-loop authority; owner may revisit. |

### Requirement → mechanism (slice 1)

| Requirement (discussion § Requirements) | Mechanism |
|---|---|
| Index package file declares `publishers`; any one match passes; unknown kind or field (incl. `sigstore`) matches nobody; none understood refuses | `IndexRoot.publishers` raw (D-1); strict per-entry parse into usable / unrecognised / invalid (D-2); refusal `publishers_unrecognized` 82 or `publishers_invalid` 65 (D-7) |
| Client checks on install, pull and on-demand installs with no local config; committed local resolves, `--offline`, `--frozen`, `ocx.lock` make no request | `AutoVerify` always built, hook stays in `setup_impl` (D-4); publishers from the local roots, never fetched; origin gate on the find fast path (D-5) |
| Missing `publishers` → no check unless `required`; `off` never checks; default `if_declared`; per registry | `signatures` (D-6), `publishers_required` 81 (D-7) |
| `keyless` pins certificate claims generically | `claims` map, name table plus raw OID (D-3) |
| `key` publisher verifies without Sigstore network | Per-candidate transparency, lazy trust root (D-13) |
| `publishers` and operator `[[trust.policy]]` both pass | Two rounds, AND (D-4) |
| `--no-verify` / `OCX_NO_VERIFY` skip with today's warning | D-9 |
| `publishers` is the attestation signer set; `ocx package sbom` takes the newest verified SBOM | D-8 |
| Previous release still installs | D-11; acceptance test with `ocx-previous` |
| E2E fixtures exist and are exercised | D-12 |
| ADR, system design, user reference state this design | This ADR, `system_design_publisher_trust.md`, `website/src/content/docs/reference/configuration.md` |

### Check table

| Index `publishers` | Operator policy | `signatures` | Install needs |
|---|---|---|---|
| present | none | `if_declared` / `required` | a listed publisher's signature |
| present | covers package | `if_declared` / `required` | listed publisher **and** operator signer |
| absent | covers package | `if_declared` | operator signer |
| absent, or no root known (lock / digest install on an empty home, D-5) | none | `if_declared` | nothing (INFO line, as today) |
| absent | any | `required` | refused, 81 `publishers_required` (`--no-verify` skips) |
| present | covers package | `off` | operator signer only |
| present | none | `off` | nothing |
| present, nothing usable | any | `if_declared` / `required` | refused, 82 or 65 |

The `signatures` values are snake_case (IC-06); the discussion's spellings were working names.

### Threat model (client verifies packages)

Until slice 4, `publishers` is as trustworthy as the index server and TLS.

| # | Threat | Verdict | Mechanism |
|---|---|---|---|
| T1 | Index host, CDN or DNS compromise | **Partial until slice 4** | A fresh client takes whatever root the attacker serves, `publishers` included. A machine with a committed root keeps its list: a tag grow never replaces it and the effective list never widens it (D-11), so an attacker's new tag signed by the attacker fails `publisher_mismatch`. `ocx index update <pkg>` adopts the served list (explicit act). Slice 4 closes it with the catalog signature. |
| T2 | Index mirror or pass-through proxy | **Covered** | The client verifies each package signature against `publishers`; the mirror holds no signing identity. A mirror that strips `publishers` is T1. |
| T3 | Registry or registry-proxy compromise | **Covered for declared packages** | A swapped digest has no listed signature. Undeclared packages are unprotected unless an operator policy covers them. |
| T4 | Stolen push credential | **Covered** | A pushed artifact lacks a listed signer's signature. |
| T5 | Publisher CI compromise | **Accepted** | Passes every identity check. Mitigated by claim pins on ref, trigger and workflow, and by yank. |
| T6 | Repository-name reuse (SlugFest class) | **Covered with ID claims** | `source_repository_identifier`, `source_repository_owner_identifier` pins ([OID] decision 8). Name-only `identity` entries stay exposed. |
| T7 | Wider trust from an unknown field or kind | **Covered for index entries; accepted for operator config** | Index entries fail closed (D-2). Operator `[[trust.policy]]` keeps tolerating unknown fields: a binary that predates `claims` parses the signer, ignores the map and matches wider. An older binary ignores index `publishers` altogether (no check). |
| T8 | Downgrade by stripping `publishers` from a root | **Partial** | Committed copies keep the field (overwrite-only merge; fetched-absent → committed, D-11). Fresh clients see none: T1. `signatures = "required"` turns a stripped root into a refusal. |
| T9 | Sigstore compromise | **Inherited** | Keyless publishers trust public Sigstore; `key` publishers do not. Per-host instances are slice 4. |
| T10 | Write access to an operator config tier | **Accepted** | Can set `signatures = "off"`; equal to replacing `[[trust.policy]]` today. `system_locked` pins it. |
| T11 | Replay of an older valid signature onto another package | **Partial** | Signatures bind digests, not names. A shared signer across packages lets an indexed row point at another package's genuine artifact; ingest may compare `docker-reference` (slice 3 hardening). |
| T12 | Replay of an older SBOM | **Covered** | Newest by log time among *verified* attestations; an unverified document never wins (D-8). |
| T13 | Content installed unverified (`pull_local`, `--no-verify`, undeclared name) then reached under a declared name | **Covered** | The origin gate (D-5): content without an origin marker for the declared repository falls through to the pull, which verifies. `ocx package verify` audits content on demand. |
| T14 | Lock or digest install on a home with no root for the package | **Accepted** | Undeclared (D-5, S-016): installs as today; `signatures = "required"` refuses 81. The owner's "`ocx.lock` makes no request" wins over a fetch for the check. |

**Known limits.** Rekor v2 entries carry no integrated time, so newest-wins (D-8) refuses them as unlogged. Slice 3's ingest parser must apply `serde_json`'s duplicate-key rule. Docs pair `build_signer_uri` with `source_repository_identifier` and mention `build_signer_digest` for reusable workflows.

### Wire, config, CLI and exit-code changes

| Surface | Change | Read-path compatibility for published packages | Contract record |
|---|---|---|---|
| `p/<ns>/<pkg>.json` | + optional `publishers` array | Older clients ignore it (no `deny_unknown_fields`); `merge_root` preserves it verbatim | Index wire; `ocx-sh/index` schema in slice 3 |
| `c/index.json`, dispatch objects, OCI manifests, package metadata | **No change** | — | — |
| `config.toml` `[registries."<registry>"]` | + `signatures` | Older binaries ignore the key; an unknown value reads as `required` | Config schema regenerates additively |
| `config.toml` `[[trust.policy]]` keyless signer | + `claims` map | Older binaries ignore it (wider match, T7) | Config schema regenerates additively |
| `ocx.toml`, `ocx.lock` | **No change** | — | — |
| CLI flags, `cli.json` | **No change** | — | — |
| `OCX_*` environment | **No change** (`OCX_NO_VERIFY` gains scope, not a value) | — | — |
| `--format json` reports | **No change** in shape | `ocx package sbom` returns one document where it refused before | `semantic` ledger entry if the compat gate flags the behaviour; otherwise none |
| Error document `error.detail` | + `publisher_mismatch` (77), `publishers_unrecognized` (82), `publishers_invalid` (65), `publishers_required` (81), classified on `RoundsError` | Additive | Detail rows, `errors.json` golden, sdkgen goldens; a ledger entry for whatever the compat differ reports |
| Exit codes on install, pull, exec, on-demand installs | New possible exits for a package that declares `publishers`: 65, 77, 78, 79, 81, 82 (and 75 for a log outage, as today) | Packages without `publishers` behave as today | Breaking-marked commit subject (`feat(trust)!: …`) |
| `ocx package verify` / `sbom` with no signer flags | Uses publishers + operator policy | Explicit flags unchanged | Same commit subject |

### Sequencing

1. **Client (this repository, slice 1).** Everything above. An entry carrying `sigstore` matches nobody until slice 4. Fully testable locally, plus the e2e dispatch.
2. **Release signing (designed, later; owner-driven).** Sign in the run that pushed: an own `release.yml` dispatched by hand from `refs/heads/main` (no cargo-dist feature relied on), a `sign` job in the reusable publishing workflow guarded by `if: github.repository_id == <id>`, self-verify against the exact identity. A separate reviewer-gated maintenance workflow signs backfills and corrected SBOMs; **its identity is listed in `publishers`**. An own package lists two entries: release workflow and maintenance workflow. No `publishers` enters an `ocx.sh` entry before its releases are signed and backfilled.
3. **Index repository (designed, later).** `publishers` in `schema/root.schema.json` (before any entry carries it), then the ingest gate: verify new or changed tag rows against the **base** `publishers`, `pull_request_target`, data-only, exit 0 the only pass; removing `publishers` is a human-review change. `ocx.sh` bot policy accepts `keyless` on public Sigstore and `key`. With the client checking, the gate is early feedback, not the boundary.
4. **Catalog signature and Sigstore instances (designed, later).** One signature over the exact bytes of `c/index.json`, served beside it, by a signer list in the `publishers` vocabulary: built in for `ocx.sh` (keyless, the index repository's deploy workflow), replaced per registry by `index_publishers`. `index_signature = "enforce" | "warn" | "off"` rolled out `off` → `warn` → `enforce`. The signed catalog carries `expires` (30 days for `ocx.sh`, re-signed every 7 days and on each change), refused on the remote-to-local write path; `index_expiry = "ignore"` opt-out; an external alert at less than 21 days. Package-file hash checked against its catalog entry. `ocx index sign --key <pem> <dir>`. `[trust.sigstore]` becomes address-keyed `[sigstore."<host>"]` plus `[registry] sigstore` / `[registries."<host>"] sigstore`; TUF mirrors move to `[mirrors]`; the `sigstore` field of a publisher entry becomes usable. `--no-verify` then also skips the index check.

### Consequences

**Positive:** every ocx client verifies declared packages with no config and no release; a mirror or proxy is never a trust point; GitLab and future forges are reached by claims; key publishers work offline from Sigstore; no keys to guard.

**Negative:**
- Until slice 4 the index server can rewrite `publishers` for fresh clients (T1).
- Rotation on a machine with a committed root needs `ocx index update <pkg>`; a release signed only by a new signer fails `publisher_mismatch` there until then. The error message names the command.
- Removing `publishers` from a root never reaches a committed or effective list (D-11); publishers rotate entries instead of dropping the field. A machine escapes with `signatures = "off"` for that registry.
- Operator `[[trust.policy]]` stays tolerant of unknown fields (T7).
- A lock or digest install on a home with no root installs undeclared (T14).
- Two verify rounds fetch referrers twice when both apply (≤ 16 candidates each).

**Risks:** an extension pin silently ignored (CVE-2026-48815 class) — mitigated by a red unit test per decode branch and one recorded GitHub-issued certificate fixture; claim names `.1.23`/`.1.24` unconfirmed on a real certificate ([OID]) — no guide recommends them until the e2e certificate confirms them.

### ADR amendments

| ADR | Amendment |
|---|---|
| `adr_trust_policy.md` | Keyless signers gain `claims`; at least one of `identity`, `identity_regexp`, `claims`. `resolve_tiered` output is ANDed with index `publishers`, never ORed. The index publisher list is not a policy tier. |
| `adr_sbom_attestations.md` | Several verified attestations of one predicate type: newest by transparency-log time wins; refusal stays for an unlogged candidate or a tie. Index packages use `publishers` as the attestation signer set when no signer flag is given. |
| `adr_offline_verify_trust_cache.md` | A `key` candidate needs no Sigstore network; the trust root resolves lazily on the first keyless candidate. |
| `adr_oci_referrers_signing_v1.md` | Auto-verify covers packages that declare `publishers`; every fetch leader verifies, and content found without an origin marker for a declared package is pulled and verified. |

## Validation

Each check shown red on the failing case and green on the passing one.

- [ ] Signed by a listed publisher installs; signed by another fails 77 `publisher_mismatch`; unsigned fails 79.
- [ ] No `publishers` installs unchanged; `signatures = "required"` fails it 81; `off` skips the publisher round.
- [ ] An unchanged binary honours `publishers` added to a root (fresh home; committed home after `ocx index update <pkg>`).
- [ ] A second entry lets releases signed either way install; removing an entry stops fresh installs of its releases.
- [ ] Only unrecognised entries (unknown kind, unknown field, `sigstore`) → 82; only invalid entries → 65; one usable entry beside unrecognised ones still matches.
- [ ] A `key` publisher verifies with Sigstore endpoints unreachable and no trust-root cache, also beside a keyless entry.
- [ ] Content installed under an undeclared name, then run under a declared name signed by nobody listed, fails 77 (S-017); a lock install on an empty home installs undeclared, 81 under `required` (S-016).
- [ ] Both `publishers` and an operator policy: installs only with both signatures.
- [ ] `--no-verify` and `OCX_NO_VERIFY=1` skip with exactly one warning line.
- [ ] `claims` matches a real GitHub-issued certificate by repository and owner ID, by name and by raw OID, and rejects the second-workflow case.
- [ ] Two verified SBOMs: `ocx package sbom` returns the newer; one unlogged candidate refuses 65.
- [ ] The previous ocx release installs a package whose root carries `publishers`.
- [ ] A committed local resolve makes zero requests; an offline frozen find of content carrying its origin marker succeeds with zero requests.

## Links

- [`system_design_publisher_trust.md`](./system_design_publisher_trust.md) — components, contracts C-001…, scenarios S-001…
- [`research_fulcio_claim_oids.md`](./research_fulcio_claim_oids.md), [`adr_trust_policy.md`](./adr_trust_policy.md), [`adr_exit_code_taxonomy.md`](./adr_exit_code_taxonomy.md), [`adr_sbom_attestations.md`](./adr_sbom_attestations.md), [`adr_offline_verify_trust_cache.md`](./adr_offline_verify_trust_cache.md), [`adr_index_indirection.md`](./adr_index_indirection.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-10-25 | architect (Opus) | Initial draft (option B) |
| 2026-10-25 | architect (Opus) | Round-1 fix pass: C1 (minimal TUF) chosen |
| 2026-11-13 | architect (Opus) | Decision Outcome replaced per the 2026-11-12 discussion: option E (client-checked `publishers`), TUF dropped, slice-1 decisions D-1…D-14, threat model rewritten for a verifying client, status Accepted. TUF decision summary, fallback, ingest-gate detail and the round-1 fix-pass table deleted |
| 2026-11-13 | hex-plan review | Round 1 fixes R-1…R-14, P-D-1, P-D-2 |
