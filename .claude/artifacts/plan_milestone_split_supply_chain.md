# Plan — Supply chain v1 milestone split + story refinement

> Proposal (proposal-first — no GitHub mutations yet). Sources: 5-agent workflow run `wf_d55e4323-c05`
> (impl audit vs branch `feat/oci-referrers-sign-verify`, split design, story critique, SOTA web research
> 2026-07-09, completeness critic). Full findings: workflow journal.

## Ground truth (verified against code)

- PR #87 = scaffold. `SignPipeline::run` / `VerifyPipeline::run` `unimplemented!()`, commands exit 78.
  **No `sigstore` dependency in Cargo.toml.** All crypto modules (fulcio, rekor, signer, bundle, oidc,
  identity matchers, `NativeTransport::{list_referrers,push_referrer_manifest}`) are typed shells.
  Real logic: validation, OIDC precedence, error taxonomy (exit 64/65/78/81/83/84), capability cache,
  referrer manifest builder, fake_sigstore.py stack (818 lines), test_sign/test_verify specs (1327 lines, skipped).
- #106 is ~80% shipped: `oci/referrer/capability.rs` (723 lines) — lazy `GET referrers` probe,
  cache `$OCX_HOME/state/referrers/<slug>.json`, 6h TTL, per ADR signing_v1 Amendment 3. Issue body describes
  a fictional `/v2/_catalog` header probe, wrong path, wrong TTL.
- **Test harness blocker**: `test/docker-compose.yml` runs `registry:2` — no OCI 1.1 Referrers API
  (landed in distribution v3). Every positive-path acceptance test in the milestone is impossible today.
  Also unverified: does ocx.sh (production default registry) support referrers?
- #105 closed as migrated → **ocx-sh/ocx-mirror#7** (verified via gh). Cross-repo edge: mirror re-sign is
  downstream consumer of the pipeline.
- Command naming drift: issues say `ocx sign`/`ocx verify`/`ocx sbom`; shipped taxonomy = `ocx package sign|verify`
  → new commands go under `ocx package` too.

## SOTA corrections (2026-07-09, web-verified)

| Claim in issues | Reality | Impact |
|---|---|---|
| sigstore-rs "upgrade to Rekor v2" | sigstore 0.14.0, pre-1.0/experimental. Keyless sign: yes. TUF root: yes. **Rekor v2: likely NOT yet** (Go/Python/cosign have it; Rust unmentioned in GA post). **Does NOT verify attestations (DSSE/in-toto)** | Spike required; pipeline targets Rekor v1 + TUF; DSSE verify = own work item |
| Rekor v1 dying soon | v1 parallel-operates; sunset needs 1-year notice, none announced. Rekor v2 endpoint rotates ~6mo → must consume SigningConfig/TrustedRoot dynamically | Rekor v1 day-one is safe |
| cosign 2.6+ | **cosign v3.1.1**: referrers-mode DEFAULT, `.sig` tags deprecated, protobuf bundle mandatory, Rekor v2 wired | OCX referrers-only design now aligned w/ cosign default; interop must target v3; pre-3.0 cosign can't see our signatures (decide if that matters) |
| CycloneDX 1.5 predicate `…/bom/1.5` | Spec at **1.7** (ECMA-424). Correct predicate = versionless `https://cyclonedx.org/bom`; BOM's `specVersion` carries version. **cargo-cyclonedx v0.5.9 emits only ≤1.5** | #100: versionless predicate, accept 1.5–1.7 by content; docs must not promise 1.6+ generation |
| SLSA v1.1 current | **v1.2 approved**; Source Track emerging. `slsa-github-generator` being superseded by GitHub artifact attestations (deprecation plan Q1 2026) | #108 guide 2: build on `actions/attest-build-provenance`, drop slsa-github-generator |
| osv-scanner shell-out or API | osv-scanner v2.0.1+ reads `.dep-v0` natively, but runtime binary dep contradicts single-binary identity. OSV.dev returns raw CVSS vectors (v3.1/v4 mid-transition), no labels | #104: native path (`auditable-info` + `/v1/querybatch` + `cvss` crate); explicit vector-preference + bucketing rule |
| in-toto v1.1 | Confirmed. Statement `_type` stays `https://in-toto.io/Statement/v1` | #100 text fix only |
| GH artifact attestations | `push-to-registry: true` pushes cosign-spec bundle as OCI referrer; verifiable by generic Sigstore tooling | No GitHub-specific verify path needed |

## Milestone split

**A — "Signing & Trust v1"** (rename existing milestone 2, keep #24 as tracker): real sigstore-rs pipeline,
capability wiring, `[trust.policy]` identity pinning, offline/trust-root story, auto-verify on install,
cosign v3 interop, test-infra registry upgrade. Standalone story: secure defaults — signed on publish,
verified by default, typosquat defeated.

**B — "SBOM, Provenance & Scanning v1"** (new milestone + new tracker issue): attestation engine
(in-toto/DSSE attach+verify), SBOM attach/discover, SLSA provenance attach/verify, OSV scan on install,
publisher CI guides, threat model. Story: what's in the binary, who built it, is it known-vulnerable.

**Decision point (user)** — SLSA #102/#103 placement:
- **Recommended: B.** Machinery-coupled to SBOM attach (same in-toto/DSSE/referrer engine — build once);
  coupling to trust policy is one optional additive `builder` field on shipped #98 schema; keeps A's
  critical path lean (A already carries the pipeline). All dependency edges stay B→A.
- Alternative: A (it's verify/trust machinery; "compromised build system" is a trust threat). Forces the
  DSSE engine into A, bloats A, #100/#101 in B become thin consumers.
- Note: moving SLSA (+ #108 provenance guides + #109) to B exceeds the literal "SBOM + scanning" mandate —
  needs explicit user sign-off.

Docs placement: #108 SBOM guide → B always. Provenance guides + #109 threat model follow SLSA. #109 = capstone
(defense table must only claim shipped defenses).

## Action table

| # | Issue | Action | Milestone | Key changes |
|---|---|---|---|---|
| 1 | PR #87 | **Merge** (after test-plan edit: manual round-trip → "exit 78 documented preview"; link N1) | A | — |
| 2 | **N1 (new)** Implement sign/verify pipeline via sigstore-rs (slice 2) | **Create — critical path** | A | Step 0: timeboxed spike (sigstore-rs: bundle v0.3 write, TUF TrustedRoot fetch, Rekor v2, offline root override). Then: endpoint lift → `oci::endpoint` (ADR Am. 2); deps workflow sigstore-rs; fill ~10 stub modules; `NativeTransport` referrers HTTP incl. `?artifactType=` filter; trust-root injection seam (`--trust-root`/env — tests can't inject fake root today); wire capability cache; flip `#[ignore]` tests + un-skip 1327-line acceptance specs; `--format json` verify output contract (global flag, no subcommand flag); website cast (deferred in followups artifact). Target **Rekor v1 + TUF root**; v2 = #107 |
| 3 | **N2 (new)** Test infra: referrers-capable registry | **Create** | A | Harness → registry:3 (or zot); keep one registry:2 as permanent ReferrersUnsupported negative fixture. Pre-flight task: confirm ocx.sh supports referrers (if not → registry workstream). Blocks all positive-path acceptance tests |
| 4 | #106 capability detection | **Rewrite** (shrink) | A | Design shipped (capability.rs, ADR Am. 3). Scope = wire cache into pipelines (`no_cache` seam exists) + error text + acceptance test vs stubbed 404 + registry:2 fixture. Delete fictional `_catalog` probe / capabilities.toml / 24h TTL. Fix impossible criterion "registry:2 supports referrers". Sequence **after** N1 first e2e slice (error surface unreachable before) |
| 5 | #98 trust policy | **Refine** | A | `identity` (exact) + `identity_regexp` (mutually exclusive) — cosign precedent; scope semantics: most-specific (longest literal prefix) wins, ANY-of among equal scopes (rotation overlap); define tier array-merge (not an existing rule); `--certificate-identity/-oidc-issuer` become optional-when-policy-matches (CLI contract change); reuse identity.rs matchers + existing VerifyErrorKind; regex crate via deps workflow; doc surfaces: configuration.md, user-guide policy authoring, exit codes |
| 6 | **N3 (new)** Offline/air-gapped verify + trust-root cache | **Create** | A | OCX_OFFLINE + policy-matched install: fail vs skip-w/-warn (decide; never silent skip); trust-root cache in `state/`, TTL/refresh cadence, `OCX_SIGSTORE_TUF_ROOT` override; verify=online-only vs install=offline-first contradiction resolved here. **Gates #99 acceptance criteria** |
| 7 | #99 auto-verify install/pull | **Refine** | A | Verify placement: **after resolve, before download** (metadata-first seam) — makes no-partial-state trivial + saves bandwidth; offline semantics from N3; flag>env precedence, WARN once/invocation; OCX_NO_VERIFY into environment.md + `Env::apply_ocx_config`; depends #98 + N1 + N3 |
| 8 | #107 sigstore-rs Rekor v2 | **Re-scope** | A | Nothing to "upgrade" — pipeline lands in N1 on Rekor v1+TUF. #107 = Rekor v2 delta only, gated on N1 spike outcome (rekor.rs docs: "v2 deferred pending sigstore-rs support"). Close into N1 if spike shows day-one support |
| 9 | **N4 (new)** cosign v3 interop suite | **Create** | A (last) | Standalone (not N1 exit gate — feasibility unproven). Step 1 spike: cosign accepts fake-stack trusted-root JSON (`cosign trusted-root create` / `--trusted-root`). Retarget **cosign 3.x** (2.6 pins deprecated read path); decide if pre-3.0 consumer compat is a goal; prereq: cosign published to reachable registry |
| 10 | **N5 (new)** Attestation engine: in-toto/DSSE attach + verify | **Create** | B (or A if SLSA→A) | DSSE PAE encoding, dsse Rekor entry type, envelope media type; `Signer::sign(&Digest)` shape doesn't fit — second payload path. **sigstore-rs does not verify attestations** → verify side hand-rolled/second crate = real work item, under-costed everywhere. Push task must surface manifest digest for subject descriptor. #100/#102 = thin predicate slices on top |
| 11 | #100 SBOM attach | **Move + refine** | B | Versionless predicate `https://cyclonedx.org/bom`, accept 1.5–1.7 via `specVersion`; attestation path only — delete raw `MEDIA_TYPE_SBOM_*;version=` constants + deprecated `attach sbom` vocabulary; path fix: `oci/referrer/media_types.rs` (media_type.rs doesn't exist); docs: cargo-cyclonedx emits ≤1.5 only; depends N1+N5 |
| 12 | #101 SBOM discovery | **Move + refine** | B | Rename → `ocx package sbom`; drop local `--json` (global `--format json`, no-divergence rule); MVP = list + `--write` dump **without** signature verification (that's N5); server-side artifactType filter from N1 transport; depends #100 |
| 13 | #102 SLSA attach | **Refine** (+move if SLSA→B) | B* | Thin predicate slice on N5; add criterion: DSSE subject digest == pushed manifest digest (catches wiring bugs); predicate ≥ SLSA v1.0 validation stays |
| 14 | #103 SLSA verify | **Refine** (+move if SLSA→B) | B* | No `--slsa` flag: policy-driven — runs when matching `[trust.policy]` declares `builder`; missing provenance + builder pinned = fail, present + unpinned = warn; command = `ocx package verify`; depends #98 schema + N5 DSSE verify (not "small additive change") |
| 15 | #104 OSV scan | **Move + refine** | B | Native only: `auditable-info` parse `.dep-v0` + OSV.dev `/v1/querybatch` + `cvss` crate — delete osv-scanner shell-out (runtime binary dep vs single-binary identity); explicit rule: prefer CVSS v4 vector when both, CRITICAL = score ≥ 9.0, no-severity = UNKNOWN never blocks; **never prompt** (backend-first) — CRITICAL = fail w/ dedicated exit code; env `OCX_NO_VULN_CHECK` (drop OCX_INSECURE_INSTALL/--yes); OCX_OFFLINE → skip WARN; coordinate hook w/ #99 seam. Only dependency-free B story — can start day 1 |
| 16 | #108 publisher CI guides | **Split + refine** | SBOM guide → B; provenance guides follow SLSA | Guide 2: `actions/attest-build-provenance` path, drop slsa-github-generator (being superseded); sequence last (copy-paste-runnable needs real flags); SHA-pinning guidance stays |
| 17 | #109 threat model | **Refine, capstone** | follows SLSA (rec: B) | Criterion: every incident date checked vs primary source (GhostAction = Sep 2025, not Jan) + lychee run; table distinguishes shipped vs planned defenses; keep policy section generic until #98 schema freeze |
| 18 | #110 acceptance tests | **Close after redistribute** | — | Monolith contradicts contract-first TDD. Tests 1–7 → acceptance criteria of owning issues (#98, #99, #100+#101, #102+#103, #104, #106); item 6 mirror → already ocx-sh/ocx-mirror#7; item 8 cosign interop → N4. Copy "reuse fake_sigstore.py, minimal fixture extensions" note into both trackers |
| 19 | #24 tracker | **Rewrite in place** as A tracker | A | Keep number ("Part of #24" refs). Trim to sign/verify/policy/auto-verify + capability; sub-issues: #87, N1, N2, #106, #98, N3, #99, #107, N4 (+#102/#103 if SLSA→A); check off #105 w/ ocx-mirror#7 link + "unblocks ocx-mirror#7" note; fix naming drift (`ocx package sign|verify`) in ALL bodies; fix "signs on push" → "signs after push", record `push --sign` as deferred non-goal; A done-criterion: website cast recorded |
| 20 | **N6 (new)** B tracker | **Create** | B | Goals: SBOM attach/discover, SLSA*, OSV scan, guides, threat model; industry-context section moves here (CycloneDX/ECMA-424, cargo-audit deprecation, in-toto v1.1 — updated per SOTA table above); explicit "Blocked by #24: N1 (pipeline), #106, #98" |
| 21 | **N7 (new, optional)** Dogfood: attach OCX's own SBOM on release | **Create (nice-to-have)** | B | adr_sbom_strategy Phase 3 — trivial once #100 lands |

## Dependency DAG (X → depends on Y)

```
A: N1 → #87-merge          N2 ∥ (parallel, before N1 acceptance tests)
   #106 → N1(first e2e)    #98 → N1    N3 → N1    #99 → #98 + N3
   #107 → N1-spike         N4 → N1 (+cosign published)
B: N5 → N1                 #100 → N5 + #106      #101 → #100
   #102 → N5               #103 → #98 + N5 + #102
   #104 → (none — day-1)   #108 → #100/#102      #109 → all B
No A→B edges. Cross-repo: ocx-mirror#7 → N1.
```

## Order

- **A**: merge #87 → N2 ∥ N1-spike → N1 → #106 → #98 → N3 → #99 → #107(delta) → N4 → docs
- **B**: #104 immediately ∥ N5 → #100 → #101 → #102 → #103 → #108 → #109

## Improvements beyond the split (summary)

1. Critical path untracked — N1 fixes; #107 was a mirage ("upgrade" of nonexistent dep).
2. Test harness can't test the milestone (registry:2) — N2; verify ocx.sh referrers support.
3. sigstore-rs can't verify attestations — DSSE verify = explicit engine issue (N5), not implied plumbing.
4. Offline-first vs online-only verify contradiction — N3 before #99.
5. Backend-first violations removed: no interactive prompts (#104), no subcommand `--json` (#101), no root-level commands (#101, #103).
6. Version pins refreshed: cosign 3.x, CycloneDX versionless predicate, SLSA v1.2, attest-build-provenance over slsa-github-generator.
7. Monolithic test issue dissolved into per-story criteria (contract-first TDD).
8. Doc surfaces enumerated per issue (environment.md, configuration.md, exit codes, user guide) per plans-must-list-docs.

## Decisions (user, 2026-07-09)

1. SLSA #102/#103 + all #108 guides + #109 → **B**.
2. Execution: **bulk apply** (done same day).
3. cosign pre-3.0 consumer compat: **dropped**, documented (floor = cosign >= 3.0).

## Applied (2026-07-09) — final issue numbers

Milestone A = milestone 2 "Signing & Trust v1" (renamed); B = milestone 4 "SBOM, Provenance & Scanning v1" (new).

| Token | Issue |
|---|---|
| N1 pipeline via sigstore-rs | #194 |
| N2 test-infra referrers registry | #195 |
| N3 offline/air-gapped verify | #196 |
| N4 cosign v3 interop | #197 |
| N5 attestation engine (DSSE) | #198 |
| N6 B tracker | #199 |
| N7 SBOM dogfood | #200 |

Rewritten: #24 (A tracker), #98, #99, #100, #101, #102, #103, #104, #106, #107, #108, #109. Closed: #110 (not planned, tests redistributed). PR #87 test plan fixed + milestone A. Rosters verified, no placeholder leaks.
