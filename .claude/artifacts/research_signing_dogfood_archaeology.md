# Research: Signing dogfood, repo archaeology

## Metadata

- Date: 2026-11-13
- Lane: history and intent (parallel lane reads current code; not repeated here)
- Repos read: `ocx-sh/ocx` (`/home/mherwig/dev/ocx`, branch `goat`, HEAD f5e6e66a4), `ocx-sh/ocx-mirror` (main 050aab5), `ocx-indexbot`, `ocx-catalog`, `index`, `ocx-mirror-sdk` (local clones, no remote reads beyond `gh` issue/run lists)
- Sources: `git log --all` grep and `-S` (via `rtk proxy git`), `gh issue/pr/run view` (read-only), ADRs and discussions under `ocx/.claude/artifacts/` and `ocx/.agents/`, mirror `.claude/artifacts/` and `.agents/discussions/`
- Read-only throughout. Statuses below are what the records say; ADR status text quoted where relevant.

## Timeline

| Date | Event | Source |
|---|---|---|
| 2026-03-13 | SBOM strategy ADR: phase 1 cargo-auditable + cargo-cyclonedx in `dist-workspace.toml` (shipped v0.2.1), phase 3 "embed SBOM as OCI referrer" | `ocx/.claude/artifacts/adr_sbom_strategy.md` |
| 2026-05-14 | Signing ADR v1: D4 "enforcing verify only", S1-F "never write fallback tag" | `ocx/.claude/artifacts/adr_oci_referrers_signing_v1.md` |
| 2026-06-13 | ocx-mirror#7 last touched, describes signing engine as nonexistent | https://github.com/ocx-sh/ocx-mirror/issues/7 |
| 2026-07-08 | #200 SBOM dogfood opened | https://github.com/ocx-sh/ocx/issues/200 |
| 2026-08-14 | Mirror owner ruling: "integrity first, this mirror carries integrity, not trust"; referrers detected, copy refused | ocx-mirror `adr_registry_mirror_sync.md` (commit 3fa477e), summarised in `research_mirror_signing_archaeology.md` |
| 2026-08-18 | Signing milestone lands (sign/verify over referrers, closes #24, #194-#197, #204-#210) | ocx commits 72a69781 / 1243a8a4 (cited in mirror archaeology note) |
| 2026-08-20 | #200 marked "Blocked": GHCR serves no Referrers API. SBOM ADR Part VI: unsigned-SBOM parity; `adr_sbom_attestations.md` supersedes strategy phase 3 | #200 body; `adr_sbom_attestations.md` L2011, L2014 |
| 2026-08-29 | Amendment 10: S1-F reversed, OCX reads and writes the referrers fallback tag, because GHCR and Docker Hub still lack the API | `adr_oci_referrers_signing_v1.md` L1067 |
| 2026-08-30 | Cosign sidecar sweep in `package copy` (#376); #392 and #403 filed | https://github.com/ocx-sh/ocx/issues/376 , https://github.com/ocx-sh/ocx/issues/392 , https://github.com/ocx-sh/ocx/issues/403 |
| 2026-09-03 | ocx-mirror signing lands on main: `sign:` block, sign every publishing leg and the index, carry signatures and attestations across `registry sync`, backfill command | ocx-mirror commits 6314555, 68047a2, 51ec150, d1e1f44, 335f631 |
| 2026-09-04 | #200, #392, #403 re-verified: NOT_STARTED / NEEDS_DECISION | comments on those issues |
| 2026-10-04 | `sdkgen-publish.yml` gets the only `sign` job in ocx (997fd9a54), then signs "by the index digest the publish job pushed" (a7cdb45f6) | ocx git log |
| 2026-10-07 | Mirror follows ocx exit-code collapse (83-87 retired, 82 Unsupported) | ocx-mirror ced1025 |
| 2026-10-25 | Discussion 1: sign own releases + default trust; four "improvised" designs rejected; handed to architect; ADR drafted with TUF option C1 | `.agents/discussions/release-signing-trust-default.md` |
| 2026-11-12 | v0.6.6 release run: `Sign the published index` job fails, exit 79 | https://github.com/ocx-sh/ocx/actions/runs/43263952070 |
| 2026-11-12/13 | Discussion 2 supersedes discussion 1: client-checked `publishers`, signed index (no TUF), own dispatched `release.yml`; e2e-package and e2e-index created | `.agents/discussions/release-signing-trust-default-2026-11-12.md` |
| 2026-11-13 | Slice 1 (client `publishers`) merged | https://github.com/ocx-sh/ocx/pull/696 (merge commit https://github.com/ocx-sh/ocx/commit/2e3750857c62d8c026d6bd980533c750c2e9d985) |
| 2026-11-13 | #694, #695, #697 filed as follow-ups; signing-dogfood discussion opened (this loop) | https://github.com/ocx-sh/ocx/issues/694 , https://github.com/ocx-sh/ocx/issues/695 , https://github.com/ocx-sh/ocx/issues/697 |

## Q1. Prior attempts and decisions

**ocx signing engine** (landed). Keyless Sigstore, cosign v3 bundle over referrers. ADR v1 D4 "no escape hatch, hard error without Referrers API". Amendment 10 (2026-08-29) reversed the S1-F fallback-tag ban; reason recorded: GHCR and Docker Hub still lack the API, refusing "no longer pressures anyone, it only means an OCX signature is invisible on the registries most users have", and cosign parity (https://github.com/ocx-sh/ocx/issues/356, closed) requires it. The same amendment records that S1-F's claimed enforcement test (`TestTransport`) never existed (an unchecked green) and says "worth a sweep".

**ocx `package copy` and sidecars** (landed, with an open gap). Sidecars `.sig/.att/.sbom` copied byte-for-byte, before the referrers gate (https://github.com/ocx-sh/ocx/issues/376, closed). Ratified "D4" of the issue-sweep plan: copy never writes the OCI fallback index tag (lost-update race, go-containerregistry#2205). Open as https://github.com/ocx-sh/ocx/issues/392, lane 3 "write the fallback" reverses that decision and needs an ADR. Note the label D4 names two unrelated decisions (signing-pipeline fallback retry vs copy-pipeline verbatim sidecars); cite with artifact name.

**ocx-mirror signing** (landed on main 2026-09-03; ADR `adr_mirror_signing.md` status text still "Proposed"). Six decisions: D1 `sign:` spec block (keyless tag, one-way door), D2 sign every publishing leg and the index, fail the package when `sign:` is set, D3 referrer copy replaces "detect and refuse" (`CopyError::ReferrersPresent` removed), D4 `ocx-mirror package pipeline sign [--force]` backfill, pre-filter by listing only (residual recorded: unvalidated SAN read is spoofable; filter is a pure function so a validating discoverer can replace it), D5 widens one `ocx_lib` symbol, D6 zot plus Sigstore compose stack in tests (`trillian-log-signer` must be named). Trust claim: mirror signs what it produces; registry-to-registry copies carry upstream signatures verbatim and add none. No verification at copy time. Reason: "two distinct claims". Mirror discussion `mirror-signing.md`: "SBOM: no, only the signatures."

**ocx-mirror#7** (https://github.com/ocx-sh/ocx-mirror/issues/7). Still OPEN, framing stale (engine now exists); superseded by the mirror ADR.

**SBOM**: `adr_sbom_strategy.md` phase 1 shipped (cargo-cyclonedx, v0.2.1); phase 3 superseded by `adr_sbom_attestations.md` (status Proposed): DSSE in-toto attestations via `ocx package attest`, plus Part VI unsigned-SBOM parity (owner ruling 2026-08-20): unsigned SBOM pushed verbatim typed by its own media type, `verified: false`, refused under Demand mode; unsigned provenance is a hard error (exit 64). `package push --sbom` exists (https://github.com/ocx-sh/ocx/issues/100 closed). #200 (own SBOM attach) never started. Provenance https://github.com/ocx-sh/ocx/issues/102 and publisher CI guidance https://github.com/ocx-sh/ocx/issues/108 open.

**Publisher trust distribution**:
- 2026-10-25 design: staged (release signing first, trust second); keyless via GitHub OIDC, no long-lived keys; "bot checks, client trusts index"; TUF over the index (option C1) in the ADR. Rejected as bespoke by owner: built-in client anchors for own families, mutable per-package `signers` list, append-only signer entries with validity windows, recording the verifying signer in `ocx.lock`.
- 2026-11-12/13 reversal (superseded the above): client verifies packages against index-declared `publishers`; "trust the index, the digest is enough" rejected (makes whoever serves the index the trust point); TUF dropped for a signed index using the same signer vocabulary, `expires` (30 days, re-signed every 7), `index_signature` warn/enforce; own dispatched `release.yml`, cargo-dist not relied on ("owner expects cargo-dist to be replaced"); backfill identity must be listed in `publishers` (the old ADR said it never enters); newest-wins SBOM selection. Status: ADR `adr_publisher_trust_distribution.md` "Accepted (goal-loop authority, 2026-11-13; the owner may revisit)". Slice 1 landed (PR #696). Slices 2 to 4 (release signing and backfill, index bot ingest gate, index signature, `[sigstore."<host>"]`) not built.
- Closed threads (do not re-propose without new evidence): subset mirror with full upstream catalog; `index_trust = "off"` as standard advice; waiving expiry for stale mirrors; per-package-file signing; second plain-key format; TUF/TOFU for the index; publisher signing own index entry (deferred to first third-party namespace).
- Five open security decisions from the first loop (`.agents/loop-security-findings-release-signing.md`): `--no-verify` overrides a system-locked `signatures = "required"` (ranked strongest), unreadable committed root blocks `--no-verify`, `index update` silently replaces committed `publishers`, lock-first install in a fresh home skips the publisher round (accepted by ADR D-5), claims-only keyless entry pins no repo/owner claim.

**The `sign` job in `sdkgen-publish.yml`** (the one existing sign job in ocx). Added 2026-10-04 in 997fd9a54 ("package and sign ocx-sdkgen alongside the CLI"); refined in a7cdb45f6 to sign `repository@<index digest>` from the publish job's output rather than a tag resolved later. Design: `build` read-only, `publish` may push, only `sign` has `id-token: write`; every job guarded `github.repository == 'ocx-sh/ocx'` because a `workflow_call` workflow's Fulcio identity names the workflow whatever the caller. Called from cargo-dist `post-announce-jobs`. Did it ever run green? Runs of `release.yml` v0.6.5 (2026-10-07, https://github.com/ocx-sh/ocx/actions/runs/37625223215) and earlier show no sdkgen jobs in the job list I queried (empty result), so I found no evidence of a green sign job. The only run I could confirm contains it, v0.6.6 (https://github.com/ocx-sh/ocx/actions/runs/43263952070), has `Push packages` success and `Sign the published index` failure: `internal signing error: manifest not found ... exit code 79`, which matches https://github.com/ocx-sh/ocx/issues/694 (ghcr.io 404s an index digest when `Accept` lacks the index media type). The same log also shows an earlier step echo `no ocx in distrib/ocx-x86_64-unknown-linux-musl.tar.gz` and `publish exposed no index digest` as script text (shell trace of the guard lines, not confirmed as emitted errors).

## Q2. Recorded hard constraints a rollout will meet again

- **GHCR has no Referrers API** (research as of April 2026, "no roadmap"; index repo research `research_ghcr_constraints.md` L58 says queries return empty; GitHub attestations push referrer-shaped manifests but are not listable). Consequence recorded in #200: self-published `ocx.sh/ocx/cli` physically lives on GHCR (`post-release-oci-publish.yml`). Mitigation in force since Amendment 10: fallback tag `sha256-<hex>` index, written by sign/attest, read by verify. SBOM ADR D6 deferred self-dogfood for this reason; #200's 2026-09-04 comment says read-back must be checked on GHCR "through the fallback tag after the first release that carries it".
- **ghcr.io image-index subjects**: `package sign/attest/verify` on an index digest fails (exit 79) because `SIGNABLE_MANIFEST_TYPES` lacks the index type; only `-p <platform>` works (https://github.com/ocx-sh/ocx/issues/694, open). A second defect: signed subject descriptor hard-codes the image media type. registry:2 and zot ignore `Accept`, so acceptance tests do not see it; the issue asks for a strict-`Accept` stub-transport test shown red first. Earlier https://github.com/ocx-sh/ocx/issues/601 (closed) recorded that no test passed `repo@<index_digest>`.
- **Candidate cap**: `MAX_SIGNATURE_CANDIDATES = 8`, `.take(8)` over registry order, truncation logged at debug only (https://github.com/ocx-sh/ocx/issues/403, open, S size, low). Mirror backfill is the named consumer; re-signing compounds against the cap. The 2026-10-25 discussion warns: re-signing later "appends referrers against the 8-candidate cap", so settle identity first. Research note: no cap exists in the spec or cosign; the 8 is ocx's own. Related: referrers listing `Link` header dropped (https://github.com/ocx-sh/ocx/issues/401).
- **Registries without referrers**: `ocx package copy` of a signed package to a referrers-less target never exits 0 (https://github.com/ocx-sh/ocx/issues/392); acceptance harness registry:2 is such a target. https://github.com/ocx-sh/ocx/issues/391 (copy reports PUTs not discoverable referrers). Mirror's own copy engine merges into the fallback index with read-back, bounded retry, `If-Match` where ETag exists, deliberately unlike ocx copy.
- **Fallback-index race**: go-containerregistry#2205 lost-update; recorded as the one constraint repeated across three artifacts.
- **Tag cascade before sign**: `package push --sign` writes the whole cascade, `latest` included, before signing; a failed sign leaves unsigned tags while reporting "no package published" (https://github.com/ocx-sh/ocx/issues/402, open). Mirror D2 chose fail-the-package and no tag advanced.
- **cargo-dist coupling**: `release.yml` is cargo-dist generated (`ci = "github"`, `post-announce-jobs = [post-release-oci-publish, docker-publish, sdkgen-publish]`, `github-custom-job-permissions` replaces defaults, so `id-token: write` must be listed per job). `post-release-oci-publish` has only `contents: read, packages: write`; a signed attach needs `id-token: write` there and forwarded through release.yml (#200 comment). `release.yml` is tag-push triggered and the other publish workflows are `workflow_call`; discussion 2 decided on an own dispatched `release.yml` from `refs/heads/main` for a constant certificate identity, and against relying on a cargo-dist feature. A `workflow_call` signer's certificate names the workflow, not the caller, hence the repository guard. #200 also noted `release.yml:237` `output` vs `outputs` typo as silently dropping SBOM assets (not re-checked).
- **Release-tag gate**: a tag is refused unless its commit has a green readiness run on the release bytes (ad72f094f, 2026-10-24); a release push to main needs a maintainer ruleset bypass (683f61e91). Production release acts stay with the owner per the dogfood discussion.
- **Public transparency log**: every keyless signing run writes a permanent public Rekor entry; e2e signing is dispatched, not per PR. Rekor v2 batching adds seconds (mirror research).
- **Backfill spoofability**: listing-only "already signed by X" pre-filter is spoofable by anyone with repo write access (unvalidated SAN); accepted residual, mirror D4b.
- **Fixture gap**: local Sigstore stack issues `email` certificates without repository/owner claims; claims tests rely on unit-built certs plus one recorded GitHub certificate; an end-to-end claims test needs a CI-type issuer (spike).
- **Exit codes**: new signing failures map into the post-collapse codes (82 Unsupported, 81 policy/offline, 79, 77, 75); mirror D2 table is historical.
- **Windows**: Authenticode/shim signing and signing GitHub tarballs and install script are explicitly out of scope in both discussions.
- **Index drift**: `ocx.sh` signing job must deploy before a client with `warn` default ships (dogfood discussion); the index client expiry alert must not live in the same repo (GitHub disables schedules after 60 idle days).
- **Backward compat**: previous ocx release must still install a package whose entry carries `publishers`; an unknown kind or field matches nobody; `publishers` stays a list for good. Already-published tags need backfill before `publishers` appears in a production entry (a declaring package's unsigned tag is refused, exit 79).

## Q3. SBOM generation for mirrored packages, PyPI flow, mirror copying referrers

- Mirror copies referrers of every artifactType (signatures, attestations, SBOMs), depth-bounded, no type filter, because a filter recreates the silent-drop defect (Harbor goharbor#23210 cited). Documented in `ocx-mirror/docs/reference/registry-yml.md` L474-475. Cosign sidecar tags `.sig/.att/.sbom` are copied verbatim, never clobbering a destination-only same-tag signature (C-062, `sidecar_conflicts` counter).
- Mirror never generates SBOMs: "No SBOM means the mirror never generates one" (discussion 2026-09), out of scope in that ADR. The later request to generate one for the PyPI case (anchore/syft considered) is new in the 2026-11-13 dogfood discussion; no earlier record of a generation attempt in any of the six repos. `ocx/crates/ocx_python/ocx_python.cdx.json` exists as a checked-in CycloneDX file; I did not find a record of its purpose.
- Mirror ADR note: PyPI, URL index and GitHub legs were all unsigned before 2026-09-03; the `pylock/pypi` env push goes through `pipeline/python_push.rs::build_env_push_args`, an `ocx package push` subprocess, so it inherits `--sign` through that argv. `design_spec_ocx_python.md` and `design_spec_pypi_layer_storage.md` contain no SBOM or syft mention (grep).
- Mirror CLI docs: backfill skip rule counts signature-typed referrers, fallback-index entries of that type, and `.sig` sidecars; attestations (`.att`) are not skip evidence (`ocx-mirror/docs/reference/cli.md` L344, truncated read).
- `anchore/syft`, `sigstore/cosign` and `cyclonedx/cargo-cyclonedx` are announced and curated in the public index repo (`index` commits, many `announce: curate anchore/syft`, `claim: ocx.sh/anchore/grant`), so the tools are installable from OCX packages.
- SBOM ADR Part VI: unsigned SBOM is labeled `verified: false` and refused under Demand mode, so an unsigned dogfood SBOM would undercut the point (#200 comment).
- Multi-SBOM: the reader previously refused several verified documents (`multiple_attestations`, exit 65); newest-wins by transparency-log time shipped in PR #696 (follow-up comment fix tracked in https://github.com/ocx-sh/ocx/issues/697). Closed https://github.com/ocx-sh/ocx/issues/386: a multi-layer `.sbom` sidecar dropped every layer after the first.

## Q4. Open issues this work would close, change or collide with

ocx-sh/ocx:
- https://github.com/ocx-sh/ocx/issues/200 (SBOM dogfood): closes. Gate decisions recorded: signed vs unsigned; index vs platform subject; `id-token` plumbing; delete the "Blocked" section.
- https://github.com/ocx-sh/ocx/issues/199 (tracking SBOM/provenance/scanning v1): parent of #200.
- https://github.com/ocx-sh/ocx/issues/694 (index subject on ghcr.io): blocks any release signing of an index; hit by the sdkgen sign job on v0.6.6.
- https://github.com/ocx-sh/ocx/issues/403 (candidate cap), https://github.com/ocx-sh/ocx/issues/401 (Link header): collide with repeated backfill and re-sign.
- https://github.com/ocx-sh/ocx/issues/392 (copy to referrers-less registry), https://github.com/ocx-sh/ocx/issues/391: collide with mirror-of-mirror and e2e flows on registry:2/GHCR; lane 3 reverses D4.
- https://github.com/ocx-sh/ocx/issues/402 (cascade before sign): changes what a "signed release" guarantee means.
- https://github.com/ocx-sh/ocx/issues/695 (pin `publishers` byte-for-byte in ocx-mirror index rewrite): test-only, ready patch recorded in the issue, not pushed.
- https://github.com/ocx-sh/ocx/issues/697 (publisher trust follow-ups: error hint, lock guide, stale test comment, page size, changelog font preload).
- https://github.com/ocx-sh/ocx/issues/673 (trust policy scope: logical vs backing repository): affects whether `publishers`/policy covers index-routed installs.
- https://github.com/ocx-sh/ocx/issues/102 (SLSA provenance attach), https://github.com/ocx-sh/ocx/issues/108 (publisher CI guidance), https://github.com/ocx-sh/ocx/issues/109 (threat model): adjacent, open.
- https://github.com/ocx-sh/ocx/issues/321, https://github.com/ocx-sh/ocx/issues/316, https://github.com/ocx-sh/ocx/issues/312, https://github.com/ocx-sh/ocx/issues/569: sign/verify robustness, open.
- https://github.com/ocx-sh/ocx/issues/508 (verify-deep release-provenance scan checks only Windows ARM64): adjacent "provenance" item.

ocx-sh/ocx-mirror:
- https://github.com/ocx-sh/ocx-mirror/issues/7 (re-sign + attest): stale, effectively done by the 2026-09-03 work, still open.
- https://github.com/ocx-sh/ocx-mirror/issues/65 (ECR 405 subject-refusal path only unit-tested): collides with registry-sync e2e.
- Searched `ocx-sh/ocx-indexbot`, `ocx-catalog`, `index`, `ocx-mirror-sdk` via `gh`: repositories do not resolve under `ocx-sh/` (indexbot, catalog) or returned nothing for the search (`index`, `ocx-mirror-sdk`); no related issue found.

Other-repo history on the signing subject:
- `ocx-indexbot`: no signature, SBOM or publishers history; bot is about announce, governance, provenance of the *requester* (GitLab CI_PROJECT_PATH gate, owner serialization, numeric-id approvals). The index ADR `adr_public_index_registry_indirection.md` L380 notes the announce doorbell credential for publishers; `research_index_announce_bots.md` records that registering a new signing key is "always human-reviewed". `research_ghcr_constraints.md` row 4 flags supply-chain signing as "Deferred" in the index ADR.
- `ocx-catalog`: no signed or SBOM badge history. `.claude/rules/product-context.md` L109-122 says supply-chain topics "matter once the catalog surfaces attestations"; `research_initial_review_2026-08-22.md` L110 lists SBOM among "upstream/non-goal" items.
- `index`: schema has no `publishers` yet (per discussion 2 current-state, checked 2026-11-12); no ingest signature gate; no `tuf/`. Catalog config and p/ package roots only.
- `ocx-mirror-sdk`: no signing or SBOM history.

negative:
- No reverted or abandoned signing commits in ocx-mirror (mirror archaeology note says none either side); ocx `-S` removal search for signing code not exhaustively run.
- No prior attempt at SBOM generation (syft or otherwise) for mirrored or PyPI packages in any repo.
- No evidence found that the sdkgen `sign` job ever ran green; no sdkgen jobs surfaced in v0.6.5 and earlier run listings I queried, and the one confirmed run (v0.6.6) failed.
- No `e2e-mirror` repository exists yet (gh lookup failed); `e2e-package` and `e2e-index` created 2026-11-12.
- No indexbot, catalog, SDK history on signature/SBOM/publishers.
- `adr_sbom_attestations.md` status is still "Proposed"; the mirror signing ADR status text is still "Proposed" though the code landed.

leads:
- `ocx/.claude/artifacts/research_cross_registry_sidecar_promotion.md` (named in mirror archaeology as fullest sidecar-vs-fallback writeup; not read).
- `ocx/.agents/loop-notes-release-signing.md` and `.agents/goals/` goal file for the first loop (not read).
- `adr_sbom_attestations.md` L2011-2092 Part VI plus `review_adr_sbom_*` files (security review of unsigned SBOM parity) for the signed-vs-unsigned gate.
- `ocx-mirror/.claude/artifacts/research_ci_signing_config_2026.md` (CI signing config) and `notes_mirror_test_design.md`.
- Verify v0.6.5-and-earlier sdkgen job presence directly (`gh run view <id> --json jobs`) and when `sdkgen-publish` first fired; also what `docker-publish` attestations (`id-token`, `attestations: write` in `dist-workspace.toml` L41) already produce on the image path, a GitHub-native attestation route that is separate from OCX signing.
- ocx-sh/ocx#546 (closed) concerns macOS ad-hoc code signing, unrelated to Sigstore; ADRs `adr_codesign_*` cover it.
