# ADR: Publisher trust distribution — minimal TUF over the index, ingest-verified releases

## Metadata

**Status:** Proposed
**Date:** 2026-10-25
**Deciders:** Michael Herwig (owner); architect draft, round-1 fix pass
**Beads Issue:** N/A — related [ocx-sh/ocx#199](https://github.com/ocx-sh/ocx/issues/199), [ocx-sh/ocx#102](https://github.com/ocx-sh/ocx/issues/102), [ocx-sh/ocx#108](https://github.com/ocx-sh/ocx/issues/108), [ocx-sh/ocx#109](https://github.com/ocx-sh/ocx/issues/109), [ocx-sh/ocx#403](https://github.com/ocx-sh/ocx/issues/403), [ocx-sh/ocx-mirror#7](https://github.com/ocx-sh/ocx-mirror/issues/7)
**Related PRD:** `prd_release_signing_trust_default.md` (handed off to architect, 2026-10-25)
**Tech Strategy Alignment:**
- [x] GitHub Actions + OIDC, Rust/Tokio; `tough` is already in `Cargo.lock` (0.22.0, line 7150) — a direct dependency, no new crate
- [ ] No deviation — an online key in a cloud KMS deviates from "Secrets: GitHub Secrets" (Q1); the environment-secret fallback does not
**Domain Tags:** security | integration | api | infrastructure
**Tier:** xhigh · **One-way door:** high · **Blast radius:** external contract (index wire format, config grammar, signer-identity scheme every own release is signed under)
**Supersedes:** none
**Amends:** `adr_trust_policy.md`, `adr_servable_index_snapshot.md`, `adr_index_indirection.md`, `adr_managed_config_tier.md`, `adr_oci_referrers_signing_v1.md`, `adr_announce_publisher_surface.md` (see [ADR amendments](#adr-amendments))
**Superseded By:** —

Citation keys:
**[ECO]** `research_publisher_trust_package_ecosystems.md` ·
**[FW]** `research_publisher_trust_frameworks.md` ·
**[OCI]** `research_publisher_trust_oci_policy.md` ·
**[GAP]** `research_publisher_trust_gapcheck.md` ·
**[MAP]** `discover_publisher_trust_architecture_map.md` ·
**[SSI]** `research_stable_signer_identity.md` ·
**[TRI]** `research_publisher_trust_round1_triage.md` (all `.claude/artifacts/`).
Every mechanism names a production adopter, or is labelled **NOVEL** or **variant** in [Labels](#labels).

## Context

The signing and verification machinery exists (`crates/ocx_sign`, `crates/ocx_trust`), but no own release is signed and no default policy exists. With no matching `[[trust.policy]]`, install skips verification with an INFO line (`crates/ocx_package_manager/src/tasks/auto_verify.rs`). Verification is opt-in everywhere.

A default client trusts only the transport on the resolution path:

- `OcxIndex` fetches `p/<ns>/<pkg>.json` directly; the root's bytes are never checked against `c/index.json`. `adr_servable_index_snapshot.md` records that "the index path's real trust boundary is the transport" (line 236).
- `c/index.json` (`CatalogDocument { format_version, packages }`) carries no version, expiry or signature. Nothing detects a rolled-back or frozen index.
- The local index is the package-tier lock (`subsystem-oci.md`): whatever a first resolve commits is trusted forever.

The index repository (`ocx-sh/index`) already has registry governance: human lanes for `repository`/`owners`/`status`/yank (G-05), new packages always human (G-04), owner-authored tag refreshes auto-merged (G-19). `render-deploy.yml` deploys to Cloudflare Pages on every merge and on a `*/15` cron.

Owner requirements (discussion artifact): (1) onboarding without an ocx release, (2) signer add/rotate/revoke without breaking old releases, including compromise, (3) offline, air-gapped and frozen-lock installs keep verifying, (4) operator override per scope, (5) a stated home, mutation path and staleness/rollback detection for trust data. Four improvised designs were rejected as bespoke: per-family client anchors; a client-consumed per-package `signers` list; append-only signer entries with validity windows; the verifying signer in `ocx.lock`.

## Decision Drivers

- **No client release for onboarding or anchor rotation** (req 1, 2).
- **Prior art only** — every mechanism cites an adopter; the owner rejects bespoke shapes.
- **Solo-maintainer operations** — one person holds every role; thresholds buy nothing ([FW] M3).
- **Offline-first** — committed resolves must stay network-free (Principle 2, `product-context.md`).
- **Don't own non-domain code** — TUF rules belong in a maintained library (`quality-core.md`).
- **Dossier constraint "no long-lived keys"** (Decisions, line 39) — written for release signing; whether it binds the index anchor is Q1.

## Industry Context & Research

**Trending:** registry-side trusted publishing on immutable CI identity (npm, PyPI, crates.io, RubyGems, NuGet; [ECO] P1). Signed, expiring repository metadata (apt `InRelease`, Nix, npm; [ECO] P2). TUF for low-churn anchor documents (Sigstore root-signing, GitHub; [FW] F10, F11). Per-publisher repository keys failed in the field (Docker Content Trust, PEP 480; [ECO] P3, [FW] F7).

### Lane disagreement

| Lane | Recommends | Against |
|---|---|---|
| [FW] | TUF Variant 2: offline root and targets, online snapshot/timestamp, one online delegation | A pinned CI identity as anchor (no chain, F15) |
| [OCI] | Signed snapshot plus a client-consumed per-package signer record | — |
| [ECO] | Signed snapshot, anchor key or TUF root delivered via the managed channel (P2a) | Per-publisher client keys |

All three agree the repository vouches for ingest-verified content. They disagree on anchor weight and on client-side publisher checks. C1 is [FW] Variant 1: its anchor, without Variant 2's offline targets role.

### Prior art per requirement

| Req | Mechanism | Named adopters | Source |
|---|---|---|---|
| 1 Onboarding | `publishers` in the root, human-gated, consumed at ingest by the bot; online-signed TUF targets carry new roots to clients | PyPI, npm, crates.io trusted publishing (ingest); RFC 3724 crates.io index as TUF target, single online key | [ECO] P1; [FW] F13 |
| 2 Change safety | ANY-of `publishers` overlap; old rows never re-verified; anchor rotates and revokes by root chaining | npm, NuGet ("existing packages aren't affected"); Sigstore and GitHub TUF roots | [ECO] P2; [FW] F1, F10 |
| 3 Offline | TUF checks only when remote data is written into the local index | apt checks `Valid-Until` when acquiring lists (`Check-Valid-Until`, sources.list(5)); Go `go.sum`; TUF update-only checks | [FW] F1, F15 |
| 4 Operator override | Per-source `index_trust` level and `index_trust_root` | cosign `initialize --root`, `gh attestation trusted-root --tuf-root`, Go `GOSUMDB`, apt `Signed-By` | [FW] F11, F15 |
| 5 Freshness, rollback | TUF timestamp (7 d), snapshot and targets versions, client datastore | Sigstore root-signing (7 d timestamp, 6 d re-sign), PEP 458 | [FW] F10 |

Caveat on apt: apt replaces lists wholesale on update; OCX keeps committed roots, like `go.sum`.

### Labels

| Mechanism | Label | Closest precedent / reason |
|---|---|---|
| Variant 1 shape (one online key over targets, snapshot, timestamp; offline root) | proposed precedent | RFC 3724 (open; 2026 prototype goal); components run at Sigstore ([FW] F10, F13) |
| `warn` never persists unverified data | **variant** | policy-controller `warn` admits; OCX cannot, because the local index is a lock |
| G-21 checks a pushed signature, not an upload token | **variant** | Trusted publishing verifies the CI token at upload ([ECO] P1) |
| `publishers` public in git; no pending/activation states | **variant** | aqua registry is public ([ECO] P4); registries keep it private. Every edit is human-reviewed, so activation windows add nothing |
| Backfill identity retired by deleting its workflow | **NOVEL** | Homebrew's backfill cutoff needs validity windows, which the owner rejected |
| Signing job guarded by `if: github.repository_id == …` | **NOVEL** | Defence in depth; the `.15` pin is the control (SLSA verifier source-repo pin, [GAP] G5) |
| Per-source update lock across fetch and commit | precedent | apt `lists/lock` |
| Levels `enforce` / `warn` / `off` | precedent | policy-controller `enforce`/`warn`, Go `GOSUMDB=off` |

### Partial precedent for the rejected designs

| Rejected design | What this ADR does | Precedent found |
|---|---|---|
| Per-family client anchors | Not adopted. One embedded TUF root for the index, the Sigstore/gh client shape. Draft B's compiled-in CI identity was this design at infrastructure scope; Homebrew's compiled-in identity costs a client release per change ([ECO] P6) | Infrastructure roots only |
| Client-consumed `signers` list | `publishers` sits in the root; only the bot consumes it | PyPI/npm enforce at upload, not on clients; aqua alone consumes client-side ([ECO] P4) |
| Validity windows | Not adopted | Windows exist for infrastructure keys only ([OCI] row 3, [FW] F17) |
| Signer in `ocx.lock` | Not adopted | PEP 751 `attestation-identities`, read by no installer ([FW] F16) |

## Considered Options

**A — operator policy only.** ocx ships a pointer to a default policy package; auto-verify checks artifacts; the index stays unsigned.
**B — index signs its catalog keylessly.** Render workflow signs `c/index.json` with its Sigstore identity, plus bespoke `version`/`expires`/high-water rules; compiled-in identity anchor.
**C1 — minimal TUF over the index** ([FW] Variant 1, RFC 3724 shape). Offline root (owner keys, threshold 1), one online key for targets, snapshot and timestamp, no delegations, no wire split.
**C2 — TUF Variant 2.** C1 plus offline targets for declarations and one online delegation for tag rows; roots split into human-lane and bot files.
**D — unsigned client-consumed declaration (aqua).** **Eliminated by owner ruling** (rejected design #2); not scored.

All of A, B, C1 and C2 keep the ingest gate (G-21) and the `github_actions` signer kind.

### Weighted matrix

Scores 1–5; weight × score; maximum 500. One clause per cell.

| Criterion (weight) | A | B | C1 | C2 |
|---|---|---|---|---|
| R1 onboarding (12) | 3 — fleet pulls a republished policy | 5 — index PR only | 5 — index PR; online key signs | 3 — each declaration needs an offline signature |
| R2 change safety (12) | 2 — compiled pointer; no anchor revocation | 3 — anchor rotation needs a client release; keyless anchor unrevocable | 5 — root chaining rotates and revokes, no release | 5 — same |
| R3 offline (10) | 4 — write-time checks only | 4 — same | 4 — same | 4 — same |
| R4 override (8) | 5 — it is the override | 5 — name a signer | 4 — operator supplies a `root.json` | 4 — same |
| R5 freshness (12) | 2 — none | 3 — OCX-own window constants | 5 — TUF timestamp/snapshot | 5 — same |
| Threats (14) | 3 — tag map unauthenticated (T1, T6, T7) | 4 — T2 total | 4 — same verdicts as B; T2 recoverable | 5 — offline targets bound T2 |
| Solo cost (12) | 4 — republish a package | 4 — no keys; daily re-sign | 3 — yearly root re-sign, one online key | 1 — offline ceremony per onboarding |
| Client complexity (8) | 5 — existing auto-verify | 3 — verifier, high-water, windows all owned | 4 — `tough` owns TUF; adapter owned | 3 — plus delegations; `tough` lacks succinct roles |
| Reversibility (6) | 4 — pointer swap | 2 — anchor in shipped binaries for years | 4 — chain changes keys, threshold, expiry | 3 — wire split is one-way |
| Fit (6) | 5 — managed + `[[trust.policy]]` end to end | 3 — new verifier seam | 3 — `tough` linked; new datastore | 2 — plus indexbot delegation |
| **Total** | **348 (70 %)** | **370 (74 %)** | **418 (84 %)** | **364 (73 %)** |

### Flip points

- C1 leads B by 48: R2 +24, R5 +24, client +8, reversibility +12; B leads R4 +8, solo +12.
- **B wins only if** the owner keeps "no long-lived keys" binding for the index anchor (C1 and C2 become infeasible), or the solo weight exceeds 60 (5× today).
- Valuing root chaining and TUF freshness at B's level (R2 = R5 = 3) ties C1 and B at 370.
- A failed `tough` spike (client 1, fit 1) leaves C1 at 382, still above B.
- C2 never overtakes C1 under these weights: its onboarding ceremony costs R1 as well as solo. It becomes relevant when third-party namespaces make bounding T2 worth that ceremony (Q2).

### Reversibility per mechanism

| Mechanism | Reversible? | How |
|---|---|---|
| Embedded `root.json` v1 | Yes, by chain | Root versions change keys, threshold, expiry; losing every root key needs a client release |
| Online key | Yes | Root revokes it; clients recover on next fetch (TUF spec 5.3.11) |
| TUF metadata format read by `tough` 0.22 | **One-way** | Spec bumps can force a client release ([FW] F18) |
| `/tuf/` layout and target names | **One-way** | Shipped clients fetch these paths |
| `github_actions` key set | **One-way** | Later narrowing keys ship as a new kind ([GAP] G4) |
| Name-based `signer_workflow` | Yes, per PR | A publisher repo rename costs one human PR to `publishers` (PyPI, crates.io parity). No client impact: the anchor is a TUF root, not a workflow name |
| RFC 3161 `SigningInstant` variant | **One-way** once shipped | Needed before Rekor v1 freezes ([GAP] G7) |
| Phase-1 SANs | **One-way per artifact** | Rekor entries are permanent; new SANs can be added |
| Rekor v1 / bundle v0.3 in the artifact verifier | **One-way** for shipped clients | Publisher path only; spike in P1 (A22) |
| Levels and override keys | Yes | Additive config |
| cargo-dist `dispatch-releases` | Yes | One config line |

## Decision Outcome

**Chosen Option:** C1 — minimal TUF over the index, ingest-verified releases. Conditional on Q1; if Q1 is "no", B with the fixes in [Fallback](#fallback-if-q1-is-no).

**Why B loses** (evidence):
1. A keyless identity cannot be revoked or rotated without a client release; [FW] F15 finds no chain for pinned CI identities. That fails req 2 for the anchor.
2. B owns a bespoke freshness protocol (Unix-seconds version, 8-day window, 1-day future bound, equivocation rule). Codex called it recreated repository-metadata machinery ([TRI] A23, C11).
3. B needed four Block/High patches (A2 Rekor-time binding, A9 root-cache isolation, A17 Sigstore egress, A19 monitoring gate) that C1 does not need.

**Why not C2:** declarations sit under the offline targets role ([FW] § Recommendation, Variant 2), so every onboarding needs the owner's offline key. With one key holder that buys no separation.
**Why not A:** the tag→digest map stays unauthenticated, so rollback, freeze and host compromise stay open.

### Fallback if Q1 is "no"

B as drafted, plus:

- freshness bound to signing time (A2): `|version − T| ≤ 1 h`, `expires ≤ T + 8 d`, where T is the Rekor integrated time or an RFC 3161 TSA time; a bundle carrying neither is refused (needs the `SigningInstant` TSA variant, [GAP] G7);
- per-source window min/max, the apt `Valid-Until-Min`/`-Max` shape ([GAP] G3);
- keyless index signing stays labelled **NOVEL** (no production keyless-signed index; TAP 18 Draft, [GAP] confirmations);
- public-good Sigstore root from TUF only, in separate storage (A9);
- a restricted-egress lane or an embedded Sigstore root (A17);
- Rekor monitoring of the index identity as an S2 gate (A19);
- the compiled-in identity labelled rejected design #1 at infrastructure scope, Homebrew as precedent.

Under this fallback, D1 (TUF repository), D2 (embedded `root.json`), D3 (`tough` client and datastore) and the TUF-specific parts of D4 (TUF refresh, the update lock through root commit) are **replaced** by the bullets above: the catalog is signed keylessly, and the trust anchor is a compiled-in identity, not a root. D4's placement rule survives, because it does not depend on the metadata format: verify only on the remote-to-local write path, through one seam, with committed resolves, `--offline`, `--frozen` and `ocx.lock` making no check. D5 (levels), D6 (`github_actions` signer kind), D7 (`publishers` and the G-21 ingest gate), D8 (sign in the run that pushed) and D10 (rollout, external watchdog) survive unchanged, since every option keeps the ingest gate and the signer kind. D9 survives except that `index_trust_root` names an operator-supplied signer identity instead of a TUF root. The threat model, the rotation table and the offline semantics are written for C1; under the fallback, the rows about root keys, the online key and TUF metadata errors are replaced by the signing-identity and freshness rules above.

### Decision summary

| # | Decision |
|---|---|
| D1 | **TUF repository over the index.** Served under `/tuf/`. Root: two offline owner keys, threshold 1, 365-day expiry. One online key signs targets, snapshot (365 d) and timestamp (7 d, re-signed daily). No delegations, `consistent_snapshot: false`. Targets: `c/index.json` and every `p/<ns>/<pkg>.json`; dispatch objects stay content-addressed below roots. |
| D2 | **Anchor = embedded initial `root.json`** for the `ocx.sh` namespace only. It rotates by root chaining; no client release for key changes. |
| D3 | **Client = `tough`** (direct dependency, unified with the fork's 0.22) in `ocx_index`. A `tough::Transport` adapter wraps `OcxIndex`'s HTTP stack (retry, SSRF guard, proxy); the fork's `ReqwestTransport` (`external/sigstore-rs/src/trust/sigstore/transport.rs:51`) is the in-tree precedent. Datastore: `$OCX_HOME/state/index-tuf/<source-slug>/`. |
| D4 | **Checks only on the remote→local write path.** Every root GET goes through one verified seam; committed resolves, `--offline`, `--frozen`, `ocx.lock` and shipped index copies make no check and no request. A per-source update lock spans TUF refresh through root commit. |
| D5 | **Levels** `enforce` / `warn` / `off` per source. `warn` reports and never persists unverified data. `off` exists only in operator config tiers. No global switch: `--no-verify`, `OCX_NO_VERIFY` and every other flag or variable leave index checks on ([GAP] G9). |
| D6 | **`github_actions` signer kind:** `owner_id` (`.17`), `repository_id` (`.15`), `signer_workflow`, `ref` all required; IDs are `u64`, read as DER UTF8String decimals. `trigger` (`.20`, an allowlist, stricter than PyPI's forbidden-trigger list) and `environment` (`.23`, emitted only when the job names one) optional. `.24` Token Subject is never pinned: its form depends on repository age since 2026-04-23. Zero or empty values fail compile. An unknown key makes that entry accept nobody; `TrustPolicy::compile` never fails the policy for it. |
| D7 | **`publishers`** (array of `github_actions` matchers) in the root: human-lane key (G-05), mandatory for new roots. G-21 verifies every new or changed tag row at ingest (hardened gate, [Ingest gate](#ingest-gate-g-21)). Old rows are grandfathered (NuGet); clients never re-verify them. |
| D8 | **Phase-1 identity: sign in the run that pushed.** A `sign` job inside each publishing workflow signs the digest its own run pushed (uv, ruff, Chainguard). Releases run via cargo-dist `dispatch-releases` on `refs/heads/main`, so the SAN is constant. No dispatched signing oracle. |
| D9 | **Operator override:** `[registries."<ns>"] index_trust` and `index_trust_root` from operator config tiers only. From a managed snapshot they, and any `index` change on an anchored namespace, apply only with a digest-pinned `[managed] source`. An explicit `index_trust_root` defaults to `enforce`. |
| D10 | **Rollout:** `off` (inert) → `warn` → `enforce`; an external watchdog is live before `warn`. The flip ships as a breaking-marked commit subject. |

### Ingest gate (G-21)

| Rule | Fixes |
|---|---|
| Runs under `pull_request_target`; reads head JSON as data, never executes head code | A5 |
| `publishers` read from the **base**; a new root's comes from head, which G-04 sends to a human anyway | A5 |
| Auto-merge classifier is an allowlist: only `tags`, `desc`, `source` changes qualify | A5 |
| A PR touching `publishers` and `tags` together is refused; "base has `publishers`, head lacks it" goes to a human | A5 |
| Pinned `ocx` (bootstrapped by digest) runs from an empty temp dir with `OCX_NO_CONFIG=1` and a generated policy | A5 |
| Exit 0 is the only pass; 75 and every other code fail the required check (rerun on outage) | A3 |
| Branch protection requires up-to-date branches on index `main`, so G-21 re-runs on every base change | A7 |
| indexbot ships before the schema change | A5 |

### Threat model

| # | Threat | Verdict | Mechanism |
|---|---|---|---|
| T1 | Index host, CDN or DNS compromise | **Covered** | Targets signed by a key the host does not hold |
| T2 | Index repo, render workflow or online key compromise | **Accepted, recoverable** | The online key is a full authority until root revokes it ([FW] Variant 1). Recovery needs no client release. Removal trigger: C2 (Q2) |
| T3 | Registry compromise | **Covered** | Every hop below targets is digest-pinned |
| T4 | Stolen publisher push credential or announce token | **Covered; Partial under a shared signer** | G-21 at ingest. Under one signer for many products, a genuine artifact can be mapped to another package (A13) |
| T5 | Publisher CI compromise | **Accepted** | Passes every identity check everywhere ([FW] M2). Mitigated by `ref` and `trigger` pins and yank |
| T6 | Rollback or freeze | **Partial** | Returning clients refuse older versions. A first-contact client (fresh CI runner) is bounded by the 7-day timestamp only |
| T7 | Mix-and-match | **Covered** | Snapshot pins targets; targets pin root hashes |
| T8 | Compromised render environment or indexbot release | **Partial** | The sign job re-renders without cache and signs only byte-identical output (A4). An indexbot release compromise equals T2; bumps are human-reviewed and digest-pinned |
| T9 | Repo-name reuse (SlugFest class) | **Covered** | `owner_id` and `repository_id` pins ([GAP] confirmations) |
| T10 | Package owner forge takeover editing `publishers` | **Partial** | Human lane (G-05) |
| T11 | Sigstore compromise | **Inherited** for publisher signatures | The index path no longer depends on Sigstore |
| T12 | Write access to an operator config tier (user tier, `$OCX_HOME`, `OCX_CONFIG`, a pinned managed payload) | **Accepted** | Equals replacing the anchor; same as `GOSUMDB` or `sources.list.d`. Only a system-tier entry locks it. Older binaries ignore `index_trust*` and verify nothing |
| T13 | Clock set back on the client | **Accepted residual** | Expired metadata looks valid; same in every TUF client |

### Rotation and revocation

| Event | Procedure | Old releases | Precedent |
|---|---|---|---|
| Publisher rotation | PR adds the new matcher (overlap); first release verifies at ingest; PR removes the old one | Untouched | npm overlap, Debian keyring staging ([ECO] T2) |
| Publisher removal or compromise | Human PR edits `publishers` (no CLI in v1) and yanks affected tags; G-21 applies to the PR | Pre-compromise rows stay vouched; local pins persist until `ocx index update` | NuGet, npm deprecate ([ECO] P2) |
| Online key rotation | Root v+1 lists the new key; owner signs offline | Unaffected | Sigstore root-signing |
| Online key compromise | Root v+1 revokes it; clients discard trusted timestamp and snapshot on key rotation (TUF spec 5.3.11) | Rows committed meanwhile persist locally; advisory asks for `ocx index update` | TUF fast-forward recovery |
| Root key loss | 1-of-2 threshold: the backup key signs root v+1 with a new second key | Unaffected | Sigstore keyholder turnover ([FW] F10) |
| Root expiry | Re-sign yearly; watchdog alerts 30 days ahead | Unaffected | Sigstore 197-day root |
| Loss of both root keys | Client release with a new embedded root | Unaffected | Same as every TUF deployment |

### Offline and freshness semantics

| Situation | Network | Check | Outcome |
|---|---|---|---|
| Committed local resolve | none | none — verified when written | proceeds |
| `--offline` | none | none | proceeds; an uncommitted tag stays `index_resolution_blocked` (81) |
| `--frozen`, `ocx.lock` | content by digest | none | proceeds; trust rests on the lock author's verified resolve (`go.sum` shape) |
| Shipped `--index` / `OCX_INDEX` copy | none | none | proceeds; grow-on-resolve from a remote source is verified |
| Online, no TUF metadata at an anchored source | yes | unsigned | `enforce`: 78 `index_trust_unsigned` |
| Online, signature, hash or length mismatch | yes | invalid | one uncached retry, then `enforce`: 65 `index_trust_invalid` |
| Online, timestamp expired | yes | expired | `enforce`: 75 `index_trust_expired` (retry later, check clock) |
| Online, version below the datastore | yes | rollback | one uncached retry, then `enforce`: 81 `index_trust_rollback` |
| Any failure at `warn` | yes | — | WARN once per source per process; resolve proceeds in memory; nothing committed |

`ocx self update` uses the remote view, so an expired index blocks it under `enforce`. Workaround: `index_trust = "warn"` for `ocx.sh` in user config, or the GitHub Releases binary.

### Phase-1 signer identity (own repositories)

| Repository | Signs | Identity (exact SAN; issuer `https://token.actions.githubusercontent.com`) |
|---|---|---|
| `ocx-sh/ocx` | `ocx.sh/ocx/cli`, sdkgen packages | `https://github.com/ocx-sh/ocx/.github/workflows/oci-publish.yml@refs/heads/main`, `https://github.com/ocx-sh/ocx/.github/workflows/sdkgen-publish.yml@refs/heads/main` |
| `ocx-sh/ocx-mirror` | ocx-mirror releases, mirror-published `ocx.sh/*` | the publishing workflow holding the `sign` job (named in P0) `@refs/heads/main` |
| `grimoire-rs/grimoire` | grimoire releases | same pattern |

Rules:

- The `sign` job lives in the reusable workflow that pushed the digest (template `sdkgen-publish.yml:209-256`). It signs only the digest its own run produced.
- Releases switch to cargo-dist `dispatch-releases`, so `release.yml` runs on `workflow_dispatch` at `refs/heads/main`. uv and ruff run exactly this ([SSI] § 1). Called workflows inherit the caller's ref, so every SAN ends `@refs/heads/main`.
- Every `sign` job carries `if: github.repository_id == <id>`: a foreign caller of a public reusable workflow gets no certificate. The old `workflow_call` ban mattered only for SAN-only pins; with `.15`/`.17` pins a reusable signer is safe ([GAP] G5).
- Root `publishers` pin `trigger = "workflow_dispatch"`, so dev publishes (push-triggered) never pass G-21.
- A structural check per repository (actionlint plus a test) asserts: every job holding `id-token: write` carries the repository guard, and `publishers` names only workflows whose `sign` job does.
- Each `sign` job self-verifies with the pinned `ocx package verify` against the exact SAN. Phase 1 is proven when that step is red on a wrong identity and green on the right one.
- The signature asserts "built by this repository's main-branch release run", not provenance and not package identity. DSSE/in-toto predicates are the deferred path to a build binding ([GAP] G5).
- **Backfill:** a separate `backfill-sign.yml` (dispatch, environment with a required reviewer). Subjects come from release records, using the last merged image-index digest per multi-platform push, never registry listings. The workflow is deleted after the run. Its SAN never enters `publishers` (old rows are grandfathered); operators may opt in via `[[trust.policy]]`.

### Wire and persisted changes

This table describes the chosen option, C1. Under the fallback, the `/tuf/` rows and the `index-tuf` datastore row do not exist and are replaced by a signature bundle beside `c/index.json`, whose location the fallback does not yet specify; the other rows hold.

| Surface | Change | Read-path back-compat | Contract ledger |
|---|---|---|---|
| `/tuf/<N>.root.json`, `timestamp.json`, `snapshot.json`, `targets.json` (served, new) | TUF 1.0 metadata | New URLs; old clients never request them | none |
| `c/index.json` | **No change** | — | — |
| `p/<ns>/<pkg>.json` | + `publishers` | `IndexRoot` has no `deny_unknown_fields` | none |
| `ocx-sh/index` `root.schema.json` | + `publishers`, required once the P3 backfill PR lands | n/a | none |
| `config.toml` | `index_trust`, `index_trust_root`; `SignerSpec` kind `github_actions` | Older binaries ignore unknown keys; an unknown level reads as `enforce` | Config schema regenerates additively |
| `error.detail` | Four slugs under existing codes 65, 75, 78, 81 | Additive | Additive slugs need no entry |
| `cli.json` | **No change** (claim flags cut) | — | — |
| `$OCX_HOME/state/index-tuf/<source-slug>/` | `tough` datastore | Deletion = TUF first contact (expiry-bounded) | Internal |
| `ocx.lock`, `ocx.toml` | **No change** | — | — |
| Exit codes | No new code; behaviour change at the `enforce` flip | — | Announced by the `!` commit subject |

### Default enforcement rollout

| Stage | Client default for `ocx.sh` | Gate to advance |
|---|---|---|
| S0 | none | Phase-1 signing live in all three repositories; backfill done; own-certificate decode gate green |
| S1 | `off` (code merged, inert) | Index serves TUF metadata; external watchdog live and green for 14 days |
| S2 | `warn` | 30 days or two releases with zero warnings in dogfood CI and the real-index acceptance probe |
| S3 | `enforce` | — |

Docker Content Trust's lesson: opt-in is unused, and scheduled expiry became mass failure ([OCI] § Content-trust deployments). Hence warn first, an alarm outside GitHub, and expiry checked only online.

### Quantified Impact

| Metric | Before | After | Notes |
|---|---|---|---|
| Requests on a committed local resolve | 0 | 0 | Acceptance test (silence principle) |
| Requests on a remote write path | root (+ dispatch) | + ≤ 4 per source per process (root probe, timestamp, snapshot, targets) | Roots are targets: no extra request per root |
| Catalog size | 13,059 B, 132 packages (measured 2026-10-25) | unchanged | `targets.json` ≈ 25 KB (estimate) |
| Freeze window | unbounded | 7 days for first contact; none for returning clients | Timestamp expiry |
| Key custody | none | 2 offline root keys, 1 online key | Q1 |
| Infra cost | $0 | $0 (environment secret) or ≈ $1/month (KMS) | Q1 |
| Recurring duties | none | daily timestamp (automated); yearly root re-sign | Watchdog covers both |

### Consequences

**Positive:**
- Default trust for `ocx.sh` with no per-publisher client state; onboarding by index PR.
- Anchor rotation and revocation without a client release.
- `ocx self update` gains index authentication.
- The index path drops its Sigstore dependency; Sigstore stays on the publisher path only.

**Negative:**
- Key custody: two offline root keys and one online key.
- Daily timestamp liveness duty; an outage over 6 days blocks new online resolves at `enforce`.
- Whole-targets fetch per process on the remote path. Ceiling ≈ 10⁴ packages (≈ 2 MB); upgrade path: hash-bin delegations or TAP 16.
- An operator pointing `ocx.sh` at a non-faithful private index must set `index_trust_root` or `off` (fail-closed).

**Risks:**
- **`tough` lag** — spec bumps broke the Rust client before ([FW] F18). Mitigation: P1 spike; pin the spec version published.
- **Extension pin silently dropped** — CVE-2026-48815 ([GAP] G6). Mitigation: red test per pinned field; golden certificate cross-checked by sigstore-go.
- **Rekor v1 freeze** — announced with a year's notice ([GAP] G7). Mitigation: P1 v2/TSA spike; `sigstore-rust` as the fallback library.

### ADR amendments

| ADR or rule | Amendment |
|---|---|
| `adr_trust_policy.md` | `SignerSpec` gains `github_actions`. An unknown key inside a known kind drops that entry with a log line; `TrustPolicy::compile` (`crates/ocx_trust/src/lib.rs:561`) never fails the policy for it. Index trust is not a `[[trust.policy]]` tier. |
| `adr_servable_index_snapshot.md` | "The real trust boundary is the transport" (line 236) is superseded for anchored sources. **DR2 amended** (line 125): the check is selected by source and by write path, not uniform over bytes. Known tension 1 dissolves: TUF hashes served bytes. |
| `adr_index_indirection.md` | A fetched root is verified before commit; a per-source update lock spans fetch and commit. The local catalog is unchanged. |
| `adr_managed_config_tier.md` | `index_trust*`, and `index` on an anchored namespace, apply from a managed snapshot only with a digest-pinned source. |
| `adr_oci_referrers_signing_v1.md` | Own releases sign under D8. **S1-H amended:** `index_trust = "off"` is an operator-tier escape hatch for index metadata only. No CLI flag or env var; artifact verify keeps no escape hatch. |
| `adr_announce_publisher_surface.md` | "Nothing claimed is trusted" (line 38) holds: `publishers` is human-lane data, verified against registry truth by G-21. |
| `subsystem-file-structure.md` `state/` contract (line 28) | `state/index-tuf/` deletion drops a source to TUF first contact, not data loss. Rule text updated in P1. |
| `workflow-release.md` | Release ceremony dispatches `release.yml` instead of pushing a tag (deferred decision 6). |

## Technical Details

### Architecture

```
publisher repo                         ocx-sh/index (git)                         Cloudflare Pages      client
──────────────                         ──────────────────                         ────────────────      ──────
release.yml (workflow_dispatch, main)  PR: tag row / publishers
  └─ oci-publish.yml                     │ G-21 (pull_request_target, data only):
       push ─> sign job (id-token,       │   pinned ocx verify <repo>@<digest>
       repo guard) ─> ghcr referrer      │   against base publishers
                                         v
                                       main ──> render-deploy.yml (workflow concurrency)
                                         guard: content changed or timestamp age ≥ 1 d
                                         render (no secrets)
                                         sign (env index-signing, online key):
                                           re-render, byte-identical gate,
                                           targets/snapshot/timestamp, verify with pinned ocx
                                         deploy ─────────────────────────────> /tuf/*.json
                                         smoke                                  /c/index.json
                                                                                /p/<ns>/<pkg>.json
external watchdog (outside GitHub) ── probes /tuf/timestamp.json, root expiry ──┘
                                                                                     │ remote write path:
                                                                                     │ tough refresh, targets verify,
                                                                                     │ under per-source lock
                                                                                     v
                                                                  $OCX_HOME/index/<source>/ (lock)
                                                                  $OCX_HOME/state/index-tuf/<source>/
```

### API Contract

```
index (served, static):
  GET /tuf/<N>.root.json   -> root, offline keys; clients chain from the embedded v1
  GET /tuf/timestamp.json  -> online key, expires <= 7 d
  GET /tuf/snapshot.json   -> online key
  GET /tuf/targets.json    -> online key; c/index.json and p/<ns>/<pkg>.json with sha256 + length
  GET /c/index.json, /p/<ns>/<pkg>.json -> unchanged bytes, verified as TUF targets

config.toml:
  [registries."ocx.sh"]
  index = "https://index.mirror.corp.example"      # re-signed mirror
  index_trust = "enforce" | "warn" | "off"
  index_trust_root = "/etc/ocx/index-mirror-root.json"
```

Rust contracts, error rows and index-side components: `system_design_publisher_trust.md` § 3, § 5.

### Data Model

```
root (p/<ns>/<pkg>.json) += "publishers": [
  { "kind": "github_actions", "owner_id": <int ≥ 1>, "repository_id": <int ≥ 1>,
    "signer_workflow": "<owner>/<repo>/.github/workflows/<file>",
    "ref": "refs/heads/main", "trigger": "workflow_dispatch" } ]     # human-review key (G-05)
```

## Implementation Plan

Dependency order (work split: system design § 12):

1. [ ] **P0 — phase-1 signing** (ocx, ocx-mirror, grimoire): `dispatch-releases`, `sign` jobs, repository guard, structural check, self-verify red/green, own-certificate decode gate (`.15`, `.17`, `.20` present and decimal), backfill.
2. [ ] **R0 — root ceremony:** two offline keys, root v1, committed to `ocx-sh/index` `tuf/`.
3. [ ] **P1 — ocx client** (inert, `off`): `tough` spike first; `github_actions` kind; `index_trust*` config and managed guard; verified root seam and lock; error rows; docs. `task satellite:verify` keeps ocx-mirror green.
4. [ ] **P1′ — index schemas** (parallel): root `publishers`; CONTRIBUTING G-05 list.
5. [ ] **P2 — indexbot:** G-21 hardened gate; tests in `tests/security/`.
6. [ ] **P3 — index signing:** signing-tool spike, `render-deploy.yml` sign job, watchdog, `publishers` PR for every existing root.
7. [ ] **P4 — `warn` release** (S2). **P5 — `enforce`** after the soak (S3).

## Validation

Acceptance tests (pytest, local static-index fixture serving a TUF repository built by the P3 tool; test roots configured via `index_trust_root`, which also covers the override):

- [ ] **Signer rotation keeps an old release verifying.** Tag `1.0` under publisher A; rotate to B; ingest `2.0` signed by B; fresh client at `enforce` resolves both. Bot test: unchanged `1.0` is not re-verified; `2.0` signed by A is refused.
- [ ] **Onboarding on an unchanged binary.** Add root Q and re-sign; the same binary resolves Q (exit 0). Mutated Q bytes: exit 65 `index_trust_invalid`.
- [ ] **Offline frozen install.** Resolve and lock at `enforce`; expire the timestamp. `--offline` and `--frozen` exit 0 with zero requests; a new online resolve exits 75.
- [ ] **Operator override.** `index_trust_root` for a fixture repo verifies; metadata signed by another key exits 65. An unpinned managed tier setting `index = ""` or `index_trust = "off"` is ignored with a WARN (A8).
- [ ] **Rollback refused.** Serve version N, then a valid N−1: exit 81. Delete the datastore: accepted as first contact.
- [ ] **Root chaining.** A binary embedding root v1 verifies a repository at root v3.
- [ ] **Online-key compromise recovery.** Far-future timestamp under key K1, then root v2 revokes K1: client accepts the K2 timestamp (spec 5.3.11).
- [ ] **Every root GET verified (A1).** One red test each: grow-on-resolve, routing grow, `ocx index update`, `--remote` resolve, `remote_view`, `ocx index catalog --remote`, prune.
- [ ] **`warn` persists nothing (A14).** Failing check at `warn`: WARN, exit 0, then `--offline` on that tag exits 81.
- [ ] **Unsigned source.** Anchored `ocx.sh` pointed at a `file://` or HTTP index without `/tuf/`: exit 78.
- [ ] **Escape hatches inert (W2).** `--no-verify` and `OCX_NO_VERIFY=1` leave index checks on.
- [ ] **Concurrent writers (A15).** Two processes refresh across a yank; the local index never regresses.
- [ ] **Silence.** A committed local resolve makes zero requests.
- [ ] **Matcher red tests.** Missing `.17`, wrong `.15`, wrong ref, wrong `.20`, duplicate extension OID, zero ID (compile error), unknown key (entry accepts nobody, sibling still matches).
- [ ] **Golden certificate.** A real own-release bundle decodes and verifies; sigstore-go verifies the same pins.
- [ ] **G-21 bot tests.** Base-read `publishers`; allowlist classifier; `publishers` + `tags` refused; base-has/head-lacks → human; exit 75 fails the check; ID-repo reuse (T9).
- [ ] **Watchdog** shown red on a stale timestamp and green on a fresh one.
- [ ] **Phase 1** self-verify red/green, one recorded run per repository.
- [ ] Security review of auth/credential paths (`security-auditor`); one Codex `sol` pass (one-way door).

## Fix-pass resolution (round 1)

Counts: **34 fixed, 7 moot under C1, 1 kept with reason** (42 rows: A1–A28 plus 14 Warn rows).

| Row | Status | Resolution |
|---|---|---|
| A1 verify seam | fixed | One verified root fetch for `fetch_root` and `fetch_root_document`'s inline GET, before memo or commit; test per entry |
| A2 pre-signed future catalogs | moot | Root revokes the online key; spec 5.3.11 discards trusted timestamp and snapshot |
| A3 neutral admits | fixed | Exit 0 is the only pass |
| A4 renderer compromise | fixed | Independent re-render, byte-identical gate; T8 regraded; indexbot bumps reviewed |
| A5 G-21 holes | fixed | [Ingest gate](#ingest-gate-g-21) rows |
| A6 optional `publishers` | fixed | Mandatory for new roots; schema-required after the P3 PR |
| A7 revocation TOCTOU | fixed | Up-to-date branch rule on index `main` |
| A8 managed protocol downgrade | fixed | Guard drops `index`/`index_trust*` from unpinned managed tiers on anchored namespaces |
| A9 trust-root cache poisoning | moot | Index path uses no Sigstore root |
| A10 signing oracle | fixed | Sign in the pushing run; `dispatch-releases` (uv, ruff) |
| A11 trigger ban too narrow | fixed | Structural check; `trigger` key (`.20`) |
| A12 `ref` optional | fixed | `ref` and `repository_id` required; example fixed |
| A13 cross-product substitution | kept | Claim narrowed to repository and run; `trigger` pin excludes dev runs; in-toto binding deferred |
| A14 audit poisoning persists | fixed | `warn` never persists |
| A15 high-water race | fixed | Per-source lock across fetch and commit (apt) |
| A16 high-water in `state/` | fixed | TUF datastore; `state/` amendment row |
| A17 offline claim false | moot | No Sigstore egress on the index path |
| A18 alarm inside workflow | fixed | External watchdog; workflow concurrency; versions only increase |
| A19 Rekor monitoring optional | moot | Index has no Sigstore identity; publisher monitoring cut ([GAP] G8) |
| A20 compile errors | fixed | `serde_json::Value` flatten; blob-verify API dropped |
| A21 unknown key errs | fixed | Entry dropped, never `Err` |
| A22 real-cert / Rekor v2 | fixed | P0 decode gate; P1 v2/TSA spike; one-way doors named |
| A23 option analysis | fixed | C1 added, re-scored, decision follows score |
| A24 NOVEL labels | fixed | [Labels](#labels); Homebrew cited; apt citation corrected |
| A25 amendments | fixed | S1-H, DR2, `state/`, announce-surface rows |
| A26 deploy before verify ([GAP] G2) | fixed | Sign job verifies with pinned ocx before deploy. No cosign on the index path; `ocx package sign` dials an explicit `rekor_url` (`crates/ocx_sign/src/sign/pipeline.rs:67`), so a public signing-config flip cannot change publisher bundles; self-verify gates publish |
| A27 operator window ([GAP] G3) | moot | No window constants: each TUF repository sets its own expiries, so an operator mirror's own root is the per-source min/max. The index path reads no Sigstore root, so no 24 h cache expiry; byte-faithful mirror route in system design § 5.2 |
| A28 empty pins ([GAP] G6) | fixed | Zero/empty fails compile; sigstore-go cross-check |
| W T6 / `permissive` | fixed | T6 Partial; `permissive` cut |
| W `--no-verify` | fixed | Never affects index checks; test |
| W override surface | fixed | T12 |
| W operator default | fixed | Explicit `index_trust_root` → `enforce` |
| W backfill | fixed | Release records, last merged digest, dedicated identity |
| W claim forge split | moot | Claim flags cut |
| W clock scan / `file://` / diagram | fixed | No clock read in OCX code; `file://` test; new index-repo work in system design § 2 |
| W P14 tests | fixed | [Validation](#validation) |
| W exit codes (IC-22) | fixed | 78 / 65 / 75 / 81, each naming the next action |
| W solo cut list | fixed | Table below |
| W stale numbers | fixed | 132 packages, 13,059 B |
| W cold runner / self update | fixed | NFR row; workaround stated |
| W rename `version` | moot | Catalog gains no field |
| W S2/S3/S5 | fixed | Duplicate OID, chain-checked leaf, full URL; grow-on-resolve verified; T13 |

### Solo-maintainer cut list

| Item | Verdict | Reason |
|---|---|---|
| `permissive` level | cut | `warn` covers rollout; TUF has no partial mode |
| `invalid_window`, future bound, equivocation | cut | `tough` enforces TUF rules |
| Staged successor anchor | cut | Root chaining |
| Managed digest-pin guard | keep | Carries the A8 fix; extends an existing guard |
| Claim flags and forge ID lookup | cut | `publishers` is hand-written in a human-lane PR; revisit at Q2 |
| `rekor-monitor` | cut | Lists entries only; reconciliation is bespoke ([GAP] G8) |
| Serving `publishers` to clients | keep | Public audit trail; costs bytes only |
| `trigger` / `environment` keys | keep | Key set is one-way ([GAP] G4); one compare each |
| High-water file, snapshot-verifier trait | cut | Replaced by the `tough` datastore |
| External watchdog | keep | Only alarm that survives a dead schedule ([GAP] G1) |

## Open questions

- [NEEDS CLARIFICATION: Q1 — Lift "no long-lived keys" for the index anchor: two offline owner keys plus one online key?]
  Recommended: yes. Root chaining is the only anchor rotation without a client release (req 2). Online key in a cloud KMS via GitHub OIDC (Sigstore GCP KMS, RFC 3724 AWS KMS); a GitHub environment secret is the golden-path fallback, at the cost of exfiltration lasting until root revokes it. If no: [Fallback](#fallback-if-q1-is-no).
- [NEEDS CLARIFICATION: Q2 — May third-party packages enter the vouched index before split signing authority (C2)?]
  Recommended: no. The first third-party namespace triggers C2 (offline targets for declarations) and the claim flags.
- [NEEDS CLARIFICATION: Q3 — Revocation for `ocx.lock`-pinned and frozen digests (deny or advisory channel)?]
  Recommended: not in v1. Locks are digest pins like `go.sum`; the yank gate covers the tag path. Upgrade path: a signed advisory target in the same TUF repository.

## Deferred owner decisions

| # | Decision | Recommended |
|---|---|---|
| 1 | Anchor shape: compiled-in CI identity vs TUF root ([TRI] Deferred 1) | TUF root (C1) |
| 2 | Owner account single point of failure | Hardware-key 2FA plus watchdog now; C2 at the first co-maintainer |
| 3 | Admins bound by required checks on index `main` and publisher repos; direct push off | Yes |
| 4 | Index-declared publishers only ever AND on top of `[[trust.policy]]`, never ANY-of at system level | Yes |
| 5 | Reviewer on every signing run, or backfill only | Backfill only; releases are owner-dispatched |
| 6 | Release trigger moves to cargo-dist `dispatch-releases` | Yes (uv, ruff) |
| 7 | TUF datastore under `state/` with the amended contract | Accept |
| 8 | S1-H amendment for operator-tier `off` | Accept |

## Links

- [`system_design_publisher_trust.md`](./system_design_publisher_trust.md) — components, contracts, NFRs, cross-repo work split
- [`adr_trust_policy.md`](./adr_trust_policy.md), [`adr_servable_index_snapshot.md`](./adr_servable_index_snapshot.md), [`adr_index_indirection.md`](./adr_index_indirection.md), [`adr_managed_config_tier.md`](./adr_managed_config_tier.md), [`adr_offline_verify_trust_cache.md`](./adr_offline_verify_trust_cache.md), [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md), [`adr_announce_publisher_surface.md`](./adr_announce_publisher_surface.md), [`adr_exit_code_taxonomy.md`](./adr_exit_code_taxonomy.md)
- Research: [ECO], [FW], [OCI], [GAP], [MAP], [SSI]; triage [TRI]
- External: TUF specification 1.0 (client workflow 5.3.11); Rust RFC 3724; `sigstore/root-signing`; `awslabs/tough`; `sources.list(5)`; Go modules reference § Authenticating modules; cargo-dist `dispatch-releases`

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-10-25 | architect (Opus) | Initial draft (option B) |
| 2026-10-25 | architect (Opus) | Round-1 fix pass: C1 added and re-scored; decision moves to C1; triage A1–A28 and Warn rows resolved; gap-check G1–G9 folded |
