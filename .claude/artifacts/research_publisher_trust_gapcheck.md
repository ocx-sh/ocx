# Research: Publisher trust ADR — SOTA gap check

## Metadata

- Date: 2026-10-25
- Domain: supply-chain security
- Triggered by: hex-architect review of `adr_publisher_trust_distribution.md` (and `system_design_publisher_trust.md`)
- Expires: 2027-04-25
- Axis: ecosystem / state-of-the-art gap check. Question: does the drafted design miss a trending pattern, a known pitfall or a current approach that `research_publisher_trust_{package_ecosystems,frameworks,oci_policy}.md` did not surface?
- Method: primary sources read as raw text where possible (Fulcio `oid-info.md`, `principal.go`, public-good `config.yaml`; sigstore-go, sigstore-python and cosign source; tuf-on-ci source; PyPI warehouse and crates.io source; the live Sigstore TUF repository `tuf-repo-cdn.sigstore.dev`, parsed locally; real Fulcio certificates from PyPI provenance, dumped with `openssl`). Blog, news and GitHub docs pages went through a page-summarising fetch tool and are marked [S] where the wording is a summary.
- Source age: everything cited is from 2025-04 or later unless marked [OLD].
- Severity scale: Block (design wrong or unsafe as written), High (will bite in the first year; fix before accept), Warn (decide before ship), Suggest (improvement).

## Direct answer

No Block-level gap. The spine (index vouches, ingest-time verification, signed `version`/`expires` catalog, remote-path-only checks, per-source operator override) holds against everything checked. Two High gaps and four Warn gaps below; most are cheap pipeline or matcher-shape decisions, and the matcher shape is a one-way door.

1. **Rekor v2 / `integratedTime`.** Freshness does not depend on Rekor time: `version`, `expires` and the high-water mark live inside the signed payload and are judged against the local clock. The one `integratedTime` dependency is certificate validity at signing time: `SigningInstant` has a single variant, `TransparencyLog(i64)` (`crates/ocx_sign/src/verify/signing_instant.rs`), and `signed_at` in `BlobVerification` is informational. Rekor v2 has no SET and no `integratedTime`; time comes from an RFC 3161 token. As of 2026-10-25 the public-good default stays Rekor v1: the live `signing_config.v0.2.json` lists only `rekor.sigstore.dev` (v1) plus the Sigstore TSA (valid from 2025-07-04), and the Jun 2026 and Oct 2026 Sigstore posts say v1 stays the default "for the foreseeable future" and the classical stack is supported until a tentative 2029-2030 turndown. A v2 signing config already ships in the same TUF repo (`signing_config_rekor_v2.v0.2.json`, log `log2025-1`, selector ANY), so the default can flip by a TUF update, not by a cosign release (Gap G2).
2. **`.15` / `.17`.** Confirmed from the Fulcio doc, the public-good issuer config and real certificates. `.15` is the immutable repository ID and `.17` the immutable owner ID, both DER `UTF8String` holding decimal digits (e.g. `447691086`, `647025`), mapped from the GitHub OIDC claims `repository_id` and `repository_owner_id`, and present on every GitHub Actions certificate from the public-good Fulcio (the issuer config templates them from those claims and gives only `environment` an empty default; the extensions date from Fulcio PR 1073, 2023-03-27; every real certificate dumped, June and October 2026, carries both). The spec says they MAY be empty for providers whose URIs are already immutable, which does not apply to GitHub. Matcher exposure: sigstore-go yes (`CertificateIdentity` embeds `certificate.Extensions`; an empty expected value is skipped, not matched), sigstore-python yes for `.15`/`.17` (`OIDCSourceRepositoryIdentifier`, `OIDCSourceRepositoryOwnerIdentifier`), cosign CLI no (only SAN, issuer and the five deprecated `--certificate-github-workflow-*` flags). `.23` (environment) exists only when the job names an environment, and neither cosign nor sigstore-python can pin it (G6).
3. **Keyless-signed repository metadata.** TAP 18 is still Draft (last modified 2023-04-06). tuf-on-ci registers a `sigstore-oidc`/`Fulcio` key type and documents Sigstore online signing as "experimental", but Sigstore's own TUF repository signs its online roles with GCP KMS (root v16: timestamp and snapshot keys are `gcpkms:` URIs, expires 2027-04-22). No production keyless-signed index or snapshot was found, so the ADR's NOVEL label is accurate. Rename drift and the scheduled-workflow trap are real (G1, G4).
4. **Freshness and offline.** The design's 7-day expiry with a daily re-sign is exactly Sigstore's own cadence (live timestamp role v817 expires 2026-11-01T01:36:40Z, observed 2026-10-25). The gaps are the operator side: a fixed 8-day cap that also binds operator anchors, and no route to give the built-in anchor an operator-supplied trusted root (G3).
5. **Trusted publishing 2026.** D6 (resolve immutable IDs at claim time) is what crates.io, PyPI (owner ID) and the OpenSSF spec do, and what the SlugFest fix moved npm to; pinning `repository_id` as well is stricter than PyPI (it lists `repository_id` as an unchecked claim) and follows the SlugFest verifier advice. That advice also says to allowlist the trigger event, and PyPI enforces a forbidden-trigger list; D5 has no trigger key (G4).
6. **Other.** Alarm shares fate with the scheduled workflow (G1); no pre-deploy verification in `render-deploy.yml` (G2); dispatch-decoupled signing carries no build binding and the NC1 rationale is incomplete (G5); `rekor-monitor` does less than the ADR says (G8).

## Gaps

### G1 — High — liveness alarm shares fate with the scheduled workflow

- Affects: ADR § Consequences/Negative ("new liveness duty"), § Decision D1; system design § 3.2 (`render-deploy.yml` `guard` + `smoke`), § 8 Availability, § 11 row "Daily re-sign silently stops".
- Evidence: GitHub documents that "In a public repository, scheduled workflows are automatically disabled when no repository activity has occurred in 60 days", that scheduled runs can be delayed ("High load times include the start of every hour"), and that they run only on the default branch. The `*/15` cron fires on the hour every hour. The smoke check that fails at < 5 days of validity lives in the same workflow, so a disabled or starved schedule silences the alarm along with the re-sign. `strict` turns that silence into new-resolve failures on day 7, the Docker Content Trust shape the ADR cites. The optional `rekor-monitor.yml` is itself a scheduled workflow with the same exposure. URL: https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows [S]
- Fix: an out-of-band dead-man's switch that reads the served `c/index.json` `expires` from outside GitHub (the owner's existing monitoring stack, or a second repository's workflow), alerting at < 5 days. Keep push and dispatch triggers on `render-deploy.yml` as the manual recovery path. Cost: one HTTP probe.

### G2 — High — bundle shape is set by the TUF signing config, not by the cosign binary; nothing verifies before deploy

- Affects: ADR § Risks "Signing-tool drift", § Phase-1 bullet "Signatures are Rekor v1 bundles"; system design § 3.2 `render-deploy.yml` (job order `sign` → `deploy` → `smoke`), § 11 row 1.
- Evidence: cosign v3 turns `--use-signing-config` and `--new-bundle-format` on by default (https://blog.sigstore.dev/cosign-3-0-available/ [S]). The Rekor v2 post says clients using the publicly distributed SigningConfig "will transition automatically" and that clients "MUST NOT hardcode" the v2 URL (https://blog.sigstore.dev/rekor-v2-ga/ [S]). The live TUF repository carries `signing_config.v0.2.json` (v1 + TSA today) and `signing_config_rekor_v2.v0.2.json` (v2 `log2025-1` valid from 2026-01-01, then v1, selector ANY, plus TSA). A pinned cosign binary therefore does not pin the log: a TUF update of the default config changes the produced bundle. Rekor v2 entries have no SET; today's verifier rejects them (`RekorSetAbsentTsaPresent`, `error.rs:190`). The design's own text conflicts: § 11 says smoke verifies "before the deploy counts", § 3.2 runs smoke after `deploy`, so an unverifiable bundle is live before anything notices. Source for the v2 pipeline pattern (secondary): NVIDIA AICR passes an explicit `--signing-config` file and fails rather than let cosign fall back to v1 (https://docs.nvidia.com/aicr/latest/contributor-guide/rekor-v-2-signing [S]).
- Fix: (a) run the pinned `ocx package verify`-equivalent on the bundle inside the `sign` job and gate `deploy` on it (the previous deployment stays live, 6 days of headroom); (b) pass a committed signing-config file to `cosign sign-blob` and review it on each Sigstore announcement. Caveat from the June 2026 post: Fulcio, TSA and v2 URLs rotate, so pin only what the verifier needs, or accept (a) alone.

### G3 — Warn — operator side of freshness and trust root

- Affects: ADR D3, D4, D9, § Offline and freshness semantics, § Data Model "Window rule"; system design § 3.1 (`SigstoreSnapshotVerifier`, `SigstoreRootSelection`), § 9 External dependencies.
- Evidence and gaps:
  1. **Fixed window binds operator anchors.** `expires − version ≤ 8 d` is checked for every anchor. A mirror operator who re-signs the index under their own anchor for a sneakernet-synced fleet cannot stamp a longer window; the only relief is `permissive`, which drops expiry checking for the source entirely. apt separates the two: `Valid-Until-Min`/`-Max` raise or lower the validity window per source ("`-Min` can be used to increase the valid time on seldom updated (local) mirrors"), and `Check-Valid-Until` is the on/off switch. Debian's snapshot service tells users to disable the check for old suites, and a global off-switch also disables replay detection on healthy mirrors, which is why apt added the per-source form. URLs: https://manpages.debian.org/unstable/apt/sources.list.5.en.html , https://snapshot.debian.org/ (search snippet; direct fetch returned 503).
  2. **Built-in anchor ignores `[trust.sigstore]` and the offline rungs.** `PublicGood` resolves through cache (24 h TTL) and TUF only; offline never reaches the TUF rung (`adr_offline_verify_trust_cache.md`). A fleet behind a byte-faithful mirror of `index.ocx.sh` (the ADR's mirror route) is "online" for the index, offline for `tuf-repo-cdn.sigstore.dev`: after 24 h, `strict` gives exit 78. The workaround (restate the public identity in `index_signers` to get `Configured`) is not stated. Copied TUF caches also expire within days, the same failure the GitLab public-sector reference hit (https://gitlab.com/gitlab-com/public-sector/reference/-/merge_requests/148, secondary).
  3. **Stale-if-error for the root.** The public-good root changes in months, not days. Using a cached `trusted_root.json` past its 24 h TTL when the CDN is unreachable (never offline-only) removes a second availability domain from every strict online resolve.
- Fix: per-source `index_max_validity` (default 8 d for the built-in anchor, operator-set for their own); apply rungs 1-3 (`--trusted-root`, `[trust.sigstore]`) to the built-in anchor when the operator names a root; stale-if-error on rung 5 for the snapshot path; one paragraph in the mirror/air-gapped guide.

### G4 — Warn — the `github_actions` key set is a one-way door and misses the trigger pin; name-based workflow pin couples to rename

- Affects: ADR D5 ("future narrowing keys ship as a new kind"), D6, D8, T9; system design § 3.1 `GithubActionsMatcher`.
- Evidence: the SlugFest write-up's verifier recommendations are: enforce `repository_id`, `repository_owner_id` and the full `job_workflow_ref` with its `@refs/...` suffix, and "allowlist expected `event_name` values" (https://workos.com/blog/npm-trusted-publishing-slugfest [S]). npm's fix also rejects `issue_comment` events; PyPI rejects a forbidden-trigger list in `_check_event_name` (https://github.com/pypi/warehouse/blob/main/warehouse/oidc/models/github.py). Fulcio exposes the event as `.20` Build Trigger (observed `release` on a tag-triggered certificate). D5 pins IDs, workflow, ref and environment but has no trigger key, and D5 makes later narrowing keys a new kind, so the choice is frozen now. `sign.yml` is dispatch-only, so `trigger = "workflow_dispatch"` would be a free extra pin on every publisher entry. Separately, `signer_workflow` embeds `owner/repo`. PyPI and crates.io also compare the repository by name (and PyPI lists `repository_id` among its unchecked claims), so for ingest-consumed `publishers` a rename costing one human PR is in line with prior art. For the compiled-in index anchor the same rename costs a client release. Fulcio builds the SAN from `job_workflow_ref` (`principal.go`: `cert.URIs = baseURL.JoinPath(w.jobWorkflowRef)`), so the name in it is the current name.
- Fix: add optional `trigger` (`.20`) now; for the built-in anchor entry only, compare the in-repository workflow path against the pinned `repository_id` and treat `owner/repo` as a label; keep the exact-SAN path for `keyless`. Do not pin `.24` Token Subject: since 2026-07-15 new repositories get the immutable `sub` form (`repo:OWNER@ID/REPO@ID:...`), older ones keep names, and both change on rename/transfer (https://github.blog/changelog/2026-04-23-immutable-subject-claims-for-github-actions-oidc-tokens [S]).

### G5 — Warn — dispatch-decoupled signing carries no build binding; NC1 rationale is incomplete

- Affects: ADR D8, T5, Open question NC1; system design § 4 row "Phase-1 identity".
- Evidence: a signature from `sign.yml` says "something on `main` dispatched this subject", not "this digest came from that commit". The certificate's `.13`/`.14` describe the dispatch ref, not the build. `GITHUB_TOKEN`-triggered `workflow_dispatch` is the documented exception that creates a run, so any step holding `actions: write` can request a signature on any digest (T5 accepts this). The SLSA model separates builder from signer and has the verifier pin the *source repository* (`.12`/`.15`), which is what makes a central reusable workflow safe: for a called workflow `job_workflow_ref` (SAN) points at the callee while the `repository` claim, and so `.12`/`.15`/`.17`, stay the caller's (RubyGems states the `job_workflow_ref` half: https://guides.rubygems.org/trusted-publishing/ [S]; Fulcio maps `repository_id` and `repository_owner_id` into `.15`/`.17`). D8's reason for banning `workflow_call` ("any repository can obtain a certificate carrying this SAN") is true for SAN-only pins (today's `keyless` kind) but not for `github_actions` pins with `owner_id`/`repository_id`; with those pins a central reusable signer is defensible.
- Fix: state in the ADR that the signature asserts "signed by the repository's signing workflow", not provenance, and that DSSE/in-toto predicates (deferred) are the path to a build binding. Reword NC1 so the ban on `workflow_call` is defence in depth for SAN-only pins, and a central reusable workflow stays an option once `repository_id` pins are mandatory. Optional cheap tightening: pass the build `run_id` as an input and have `sign.yml` check that run's workflow, SHA and conclusion through the API before signing.

### G6 — Warn — one verifier family guards every pin; independent cross-check is thin

- Affects: ADR § Validation "Extension pins go red", § Risks "Extension pin silently dropped"; system design § 3.1 `github_actions_matches`.
- Evidence: CVE-2026-48815 is the live example: the sigstore npm package (≤ 4.1.0, fixed 4.1.1) accepted `certificateOIDs` and never used them (https://corgea.com/research/cve-2026-48815-sigstore-js-certificateoids-verification-bypass , https://www.wiz.io/vulnerability-database/cve/cve-2026-48815). sigstore-go's matcher skips an empty expected field by design (`CompareExtensions`: "if the expected field is empty, skip it"), so an empty or zero pin in `github_actions` must be a compile error in ocx (the API examples use `owner_id = 0`). The independent tools cannot pin everything: cosign's CLI has no flag for `.15`/`.17`/`.23`; sigstore-python's policy classes stop at `.22` (no `.23`); only sigstore-go reaches them through `certificate.Extensions`. URLs: https://github.com/sigstore/cosign/blob/main/cmd/cosign/cli/options/certificate.go , https://github.com/sigstore/sigstore-python/blob/main/sigstore/verify/policy.py , https://github.com/sigstore/sigstore-go/blob/main/pkg/verify/certificate_identity.go
- Fix: the golden-bundle test also runs in a second implementation (sigstore-go, or sigstore-python for `.15`/`.17`) against the same pins; reject zero or empty IDs at compile; keep the red test for "certificate without `.23` fails an `environment` pin" (the extension is conditional: https://github.com/sigstore/fulcio/issues/1845 closed 2025-11-16; real certificates confirm it, `.23` = `release` on pip-audit 2.10.1 and absent on sigstore-python 4.5.1).

### G7 — Suggest — Rekor v2 / TSA readiness path

- Affects: ADR § Phase-1 ("this build cannot verify Rekor v2 or TSA-only bundles"), § Risks; system design § 3.1 `BlobVerification.signed_at`.
- Evidence: Rekor v1 "will eventually" be frozen with one year's notice (https://blog.sigstore.dev/rekor-v2-ga/ [S]); the Sigstore API policy asks at least 6 months for GA deprecations (https://docs.sigstore.dev/about/api-stability/ [S]); Jun 2026: v1 stays default; Oct 2026: PQC needs a separate stack in mid-2027, "Rekor v1 will remain the default log", classical instance to a tentative 2029-2030 turndown, and a later bundle revision built on `c2sp.org/tlog-proof` (https://blog.sigstore.dev/rekor-evolution/ [S], https://blog.sigstore.dev/pqc-sigstore/ [S]). The Jun 2026 post lists sigstore-rs (the library behind ocx's fork) and sigstore-ruby as not supporting Rekor v2; `sigstore/sigstore-rust` (v0.15.1, 2026-10-23, separate crates for TSA, Rekor client with v2 tests, TUF, bundle) is listed as compatible. The live trusted root already carries the TSA (`timestamp.sigstore.dev`, since 2025-07-04) and the v2 log key (`log2025-1`, ed25519, since 2025-09-23).
- Fix: no change to the decision. Add a tracked item to take RFC 3161 verification into `SigningInstant` (a second variant) before v2, because it removes the SET dependency and today's default bundles already carry both a SET and a TSA token; rename `signed_at` to a verified-time field sourced from either; note sigstore-rust as the alternative to the fork when v2 becomes unavoidable.

### G8 — Suggest — `rekor-monitor` does less than the ADR says

- Affects: ADR § NOVEL paragraph ("Detection rides Rekor"), T2, T5; system design § 3.2 `rekor-monitor.yml` ("opens an issue on an entry with no matching workflow run").
- Evidence: the tool matches identities (SAN regex, optional OID matchers) on `hashedrekord` entries and starts from the latest checkpoint; it reports matches, it does not compare them with workflow runs, and it is a scheduled workflow (hourly recommended). The reconciler (log entries vs GitHub runs) is bespoke work. URL: https://github.com/sigstore/rekor-monitor/blob/main/README.md
- Fix: describe the monitor as "lists every signature under the identity"; budget the run-vs-entry reconciliation, or drop the "unexpected entry" claim until it exists. Same schedule exposure as G1.

### G9 — Suggest — keep expiry overrides per source (no global switch)

- Affects: ADR D9.
- Evidence: apt's `Check-Date=no` also disables `Check-Valid-Until`, and global `Acquire::Check-Valid-Until=false` is the folk fix for snapshot archives (sources.list(5), snapshot.debian.org above). D9 already limits `index_verification` to per-namespace operator config; keep it that way, with no `OCX_*` variable or CLI flag that disables expiry for every source, and name the clock in `index_snapshot_expired`/`..._invalid_window` as planned.

## Confirmations

- **Freshness independent of Rekor time** (D1, D3): signed payload plus local clock; the certificate-validity path reads no clock by construction (`signing_instant.rs` scan test). Matches the TUF fixed-start-time and apt `Valid-Until` models.
- **7-day expiry, daily re-sign** (D1): same as Sigstore's own TUF timestamp role (live: version 817, expires 2026-11-01T01:36:40Z on 2026-10-25). URL: https://tuf-repo-cdn.sigstore.dev/timestamp.json
- **Extension semantics and presence** (D2, D5): `.15` Source Repository Identifier and `.17` Source Repository Owner Identifier, DER UTF8String of the immutable ID; the public-good GitHub issuer config maps `repository_id`, `repository_owner_id` and `environment` to them. Real certificates (pip-audit 2.10.1, 2026-06-10: `.15` = 402468744, `.17` = 647025, `.23` = release; sigstore-python 4.5.1, 2026-10-20: `.15`/`.17` present, `.23` absent) confirm the parsing model "DER UTF8String of decimal digits" and "missing `.23` = mismatch". URLs: https://github.com/sigstore/fulcio/blob/main/docs/oid-info.md , https://github.com/sigstore/fulcio/blob/main/config/identity/config.yaml , PyPI provenance API (`https://pypi.org/integrity/pip-audit/2.10.1/<wheel>/provenance`).
- **Resolve immutable IDs when the declaration is created** (D6, T9): OpenSSF "Resurrection and Rename Attacks" section; crates.io resolves `repository_owner_id` through the GitHub API at config creation and rejects tokens with another owner ID; PyPI stores and compares `repository_owner_id` plus repository name, workflow and event (its `repository_id` is in `__unchecked_claims__`, so D6 pinning both IDs is the stricter shape); npm's post-SlugFest fix binds to immutable identity after the first successful publish, and GitHub expires unvalidated npm configurations after 48 h (2026-10-02). URLs: https://github.com/rust-lang/crates.io/blob/main/src/controllers/trustpub/github_configs/create.rs , https://github.com/rust-lang/crates.io/blob/main/src/controllers/trustpub/tokens/exchange/mod.rs , https://github.com/pypi/warehouse/blob/main/warehouse/oidc/models/github.py , https://helpnetsecurity.com/2026/10/13/slugfest-npm-trusted-publishing-flaw
- **IDs as `u64`**: crates.io parses the owner ID as `i32` (`exchange/mod.rs:145`), a latent overflow the `u64` choice avoids.
- **Dispatch exception** (D8): "With the exception of `workflow_dispatch` and `repository_dispatch`, other `GITHUB_TOKEN`-triggered events do not create workflow runs at all." URL: GitHub events docs above.
- **Environment for the signing job** (D8): PyPI's security model recommends a dedicated environment with protections; Fulcio now records it in `.23`. URL: https://docs.pypi.org/trusted-publishers/security-model/
- **NOVEL label** (ADR § Industry Context): TAP 18 Draft since 2021 (last modified 2023-04-06); tuf-on-ci `sigstore-oidc` is wired (`KEY_FOR_TYPE_AND_SCHEME[("sigstore-oidc", "Fulcio")]`) but its docs call Sigstore online signing experimental and "not supported by all TUF client libraries"; the Sigstore TUF online roles are KMS keys. URLs: https://github.com/theupdateframework/taps/blob/master/tap18.md , https://github.com/theupdateframework/tuf-on-ci/blob/main/docs/ONLINE-SIGNING-SETUP.md , https://github.com/theupdateframework/tuf-on-ci/blob/main/repo/src/tuf_on_ci/_repository.py
- **Extension-pin red tests** (Risks): CVE-2026-48815 is real and is the exact failure the ADR cites.
- **Closes a hole in the frameworks research**: its note that `sigstore-oidc` in tuf-on-ci was not searched is resolved: present in `repo/src/tuf_on_ci/_repository.py` and `signer/src/tuf_on_ci_sign/_user.py` (human signers use `sigstore:?ambient=false`).

## Not found / not checked

- No production system signing an index or snapshot keylessly (searched Sigstore docs, tuf-on-ci deployments, PyPI/npm/crates.io/RubyGems docs). GitHub's tuf-on-ci repository key types were not inspected.
- No primary-source incident history for GitHub OIDC or Fulcio outages; the design's 6-day headroom is the only mitigation evidence. The Sigstore public-good SLO was not checked.
- Whether `sigstore-rust` matches `.15`/`.17` is unverified (code search found no OID constants).
- The OpenSSF spec text was read from the repository; PyPI's security-model page itself does not mention repository IDs (the warehouse source shows owner ID checked, repository ID not).
- Pages marked [S] passed through a summarising fetch tool; quoted sentences there are the tool's quotes, not a raw read.

## Sources

Fulcio and certificates
- https://github.com/sigstore/fulcio/blob/main/docs/oid-info.md
- https://github.com/sigstore/fulcio/blob/main/pkg/identity/github/principal.go
- https://github.com/sigstore/fulcio/blob/main/config/identity/config.yaml
- https://github.com/sigstore/fulcio/issues/1845
- https://pypi.org/integrity/pip-audit/2.10.1/ and https://pypi.org/integrity/sigstore/4.5.1/ (provenance endpoints; certificates dumped locally)
- https://github.com/sigstore/sigstore-go/blob/main/pkg/verify/certificate_identity.go
- https://github.com/sigstore/sigstore-go/blob/main/pkg/fulcio/certificate/summarize.go
- https://github.com/sigstore/sigstore-python/blob/main/sigstore/verify/policy.py
- https://github.com/sigstore/cosign/blob/main/cmd/cosign/cli/options/certificate.go
- https://corgea.com/research/cve-2026-48815-sigstore-js-certificateoids-verification-bypass
- https://www.wiz.io/vulnerability-database/cve/cve-2026-48815

Rekor, signing config, timestamps
- https://blog.sigstore.dev/rekor-v2-ga/
- https://blog.sigstore.dev/rekor-evolution/
- https://blog.sigstore.dev/pqc-sigstore/
- https://blog.sigstore.dev/cosign-3-0-available/
- https://docs.sigstore.dev/about/api-stability/
- https://docs.sigstore.dev/about/bundle/
- https://tuf-repo-cdn.sigstore.dev/ (timestamp.json, 14.targets.json, 16.root.json, targets `signing_config.v0.2.json`, `signing_config_rekor_v2.v0.2.json`, `trusted_root.json`)
- https://docs.nvidia.com/aicr/latest/contributor-guide/rekor-v-2-signing (secondary)
- https://github.com/sigstore/rekor-monitor/blob/main/README.md
- https://github.com/sigstore/sigstore-rust , https://github.com/sigstore/sigstore-rs

TUF
- https://github.com/theupdateframework/taps/blob/master/tap18.md
- https://github.com/theupdateframework/tuf-on-ci (README, docs/ONLINE-SIGNING-SETUP.md, repo/src/tuf_on_ci/_repository.py)

Freshness and offline
- https://manpages.debian.org/unstable/apt/sources.list.5.en.html
- https://snapshot.debian.org/ (search snippet only; direct fetch 503)
- https://gitlab.com/gitlab-com/public-sector/reference/-/merge_requests/148 (secondary)
- https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows

Trusted publishing
- https://workos.com/blog/npm-trusted-publishing-slugfest
- https://helpnetsecurity.com/2026/10/13/slugfest-npm-trusted-publishing-flaw
- https://github.com/ossf/wg-securing-software-repos/blob/main/docs/trusted-publishers-for-all-package-repositories.md
- https://docs.pypi.org/trusted-publishers/security-model/
- https://github.com/pypi/warehouse/blob/main/warehouse/oidc/models/github.py
- https://github.com/rust-lang/crates.io/blob/main/src/controllers/trustpub/tokens/exchange/mod.rs
- https://guides.rubygems.org/trusted-publishing/
- https://github.blog/changelog/2026-04-23-immutable-subject-claims-for-github-actions-oidc-tokens
