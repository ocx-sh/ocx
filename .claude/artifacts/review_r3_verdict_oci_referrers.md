# Code Review: feat/oci-referrers-sign-verify (round 3)

## Summary

- **Verdict: Request Changes**
- **Tier:** max
- **Baseline:** main (default)
- **Target:** HEAD (branch: feat/oci-referrers-sign-verify; 9 commits ahead)
- **Diff:** 64 shipped files, +8 146 / −19 lines, 5 subsystems (oci, package_manager, cli, tests, website)
- **Cross-model:** ran (Codex, code-diff scope, one-shot)
- **Architect-flagged boundary concerns:** 1 Block (`Client::transport()` one-way door, F1 endpoint cross-subsystem leak)
- **SOTA gaps:** 2 Block (Rekor v2 process, TUF rotation SLA), 3 High (bundle annotation, regexp identity, CLI affordance), 1 High (SET cross-check)

### Tally

| Tier | Stage 1 | Stage 2 | Codex | Total |
|---|---|---|---|---|
| Block | — | 4 | — | **4** |
| High | 1 | 14 | 1 | **16** |
| Warn | 11 | 17 | 3 | **31** |
| Suggest | — | ~10 | — | **~10** |
| Deferred | 5 | 14 | 1 | **20** |

---

## Stage 1 — Correctness

### Spec-compliance (post-Implement traceability)

- **T2 (Actionable, High):** `error_envelope.rs:199` hard-codes `detail: None`. PRD freezes `error.detail` as dispatchable field for every error kind in C-S1-1. Reachable today via `OfflineSignRefused` and `InvalidEndpointUrl`. Add `kind_detail()` method on `*ErrorKind` enums; populate in `render_error_envelope`. Closes the only spec→code gap not blocked on Phase 5c.
- Deferred (5): all Phase 5c-gated (push state machine, identity primitives, re-sign idempotency, MAX_BUNDLE_SIZE rationale anchor, cosign verify interop).

### Test Coverage (Specify-phase adequacy)

- **Block-tier actionable:** Fulcio failure-mode test (no `set_failure_mode` on FakeFulcio) [#1]; expired-cert fixture for PRD FR-19 [#2]; verify pipeline panic-message inline test mirroring sign side [#3]; OIDC token provider unit tests (file/stdin/env/dispatcher + token redaction guard) [#4]; envelope `error.detail` golden test for reachable variants [#9].
- **High actionable (5):** GHA missing id-token test, Fulcio 503 → exit 69, Rekor sign-side outage, NFR-5 debug-log token leak grep, FR-16 sign-side JSON envelope assertion.
- **Warn:** runner `env_overlay` untested by acceptance tests, `--identity-token` rejection match too broad, fake_sigstore.SET TODO unguarded by test, fake key-material determinism, capability cache TTL design-drift bug (FR-17 says 24h/1h split, code is 6h flat).

---

## Stage 2 — Adversarial panel

### Quality (CLI-UX lens)

- **Actionable (6):** `package_verify.rs`/`PackageVerify` naming break (`verify.rs`/`Verify` diverges from `package_*` sibling pattern); `MAX_BUNDLE_SIZE_BYTES` not in ADR; `DEFAULT_FULCIO_URL`/`DEFAULT_REKOR_URL` duplicated across CLI; `BundleTooLarge` variant absent (oversized bundle routes through `Internal` losing structure); discarded `validated_endpoints()` result; verify command lacks symmetry note about absent `--fulcio-url`.
- **Deferred (4):** single-table rendering for nested JSON; empty-string rejection on identity flags; panic-vs-cancel diagnostics in `spawn_blocking`; silent fall-through on unsupported targets in token-file gate.

### Security

- **B1 Block actionable:** `--identity-token-file` TOCTOU bypass — `tokio::fs::File::open` follows symlinks; descriptor-side `fstat` is correct but attacker swapping symlink between `open` and `read` wins. Fix: `std::fs::OpenOptions::custom_flags(libc::O_NOFOLLOW)` + `tokio::fs::File::from_std`; add `meta.uid() == geteuid()` check; reject symlinks in docs (CWE-367).
- **H1-H4 High:** identity-matching canonicalisation drift (byte-equal vs trailing-slash vs case-folding contract unwritten); subject-digest binding missing from `VerifyResult`/`VerifyContext` (signature-substitution attack class, CWE-345); SSRF guard gaps (`localhost.`/`LocalHost.localdomain` bypass, DNS-rebinding window); token leak via `SignErrorKind::Internal(Box<dyn Error>)` `Display` chain reaching envelope.
- **Warn (3):** offline verify exit-code asymmetry vs sign; Windows ACL claim in capability cache doc-comment; bundle chain-cert length + per-cert size caps for hostile referrer DoS.

### Performance

- **Warn (3):** `trust_root.rs:117-141` PEM parser does upfront `from_utf8` of entire input + `block.contents().to_vec()` clone per block; `capability.rs::probe` two `.to_string()` allocations per call (low-traffic, low priority); error envelope `format!("{err:#}")` runs on every error path (one-shot, not hot).
- **Pass:** No `MutexGuard` across `.await`; no orphan `tokio::spawn`; `spawn_blocking` correctly used for tempfile rename; HTTP client reuse intact through `Clone` of `Client` (shared `reqwest` pool).

### Documentation

- **Critical:** `### package verify` rendered at wrong heading level in `command-line.md:1248` — appears as peer of `package`, not subcommand. ToC and anchors break.
- **High (2):** JSON output shape undocumented for both `package sign` and `package verify`; success envelope fields, error envelope shape, `data.identifier` vs `data.subject` asymmetry all absent from reference page.
- **Medium (3):** `OCX_IDENTITY_TOKEN` out of alphabetical order in `environment.md:150`; `in-depth/signing.md` opens with implementation internals not user goal (violates docs-style.md rule); signing-flow summary missing token-precedence cross-ref.
- **Warn (2):** preview-warning understates the panic (currently `unimplemented!()` not clean exit); verify section lacks symmetry note about absent `--fulcio-url`.

### Architecture (adversarial)

- **B1 Block deferred:** `Client::transport()` accessor one-way door — `SignContext::transport: &dyn OciTransport` is a `pub` field of a `pub struct`, locking Phase 5c to one of three options that hardens by accretion. Requires ADR amendment to `adr_oci_referrers_signing_v1.md` §"Context injection" before first Phase 5c PR.
- **F1-F3 High actionable:** `oci::verify::error::UrlRejection` imports from `oci::sign::endpoint` (lift to `oci::endpoint` peer module); `AmbientProvider::detect()` as static-trait blocks heterogeneous dispatch (flip to instance method or drop trait); `DispatchingTokenProvider::new(override, no_tty)` signature too narrow for state machine the docs claim it owns.
- **Warn (3):** `ReferrerManifest::to_canonical_json` returns `SignErrorKind` coupling neutral primitive to sign errors; `Context::online_context()` exposes `&Client` shape past facade; `pub(crate)` overuse on `oci::sign`/`oci::verify` modules contradicts `quality-rust.md` warn-tier.
- **G1 ADR realization gap:** capability cache silently introduces fourth tier (`state/`) into three-store architecture; needs ADR + `arch-principles.md` + `subsystem-file-structure.md` amendment.

### SOTA / Technical Soundness (researcher)

- **Block (2):** Rekor v2 client-support tracking — OCX not on sigstore-rs upstream list, no open issue, TUF gate misframed in ADR Risks; TUF rotation SLA undefined — `production()` vs `from_tuf()` undecided, `EmbeddedAssetMissing` is current production state.
- **High (4):** `dev.sigstore.bundle.content` layer annotation absent from ADR manifest shape (cosign verify interop hazard); `--certificate-identity-regexp` not in v2 surface list despite known practical friction; `ocx verify` typo has no clap affordance; SET cross-check (canonicalizedBody.publicKey + data.hash) not in Phase 5c checklist (cosign GHSA-whqx-f9j3-ch6m fix pattern).
- **Warn (4):** `SIGSTORE_ID_TOKEN` consumed as GitLab ambient vs cosign's explicit-override (docs gap); `--no-tty` flag wiring deferred; GHCR error message lacks named alternatives; capability TTL ADR-code drift (6h flat code vs 24h CI / 1h interactive ADR).

---

## Cross-Model Adversarial (Codex)

Codex caught issues all 6 Claude reviewers missed:

1. **High actionable — credential leak in `validate_sigstore_url` parse-error path** (`oci/sign/endpoint.rs:67`). `Url::parse(raw).map_err(|e| UrlRejection::new(format!("malformed URL \`{raw}\`: {e}")))` formats raw input **before** the userinfo scrubber runs. A malformed `https://user:pass@…` leaks credentials into stderr / JSON envelope. **Fix:** sanitize userinfo pre-parse, or omit `raw` from parse-error text (structural message only). Codex correctly flagged that doc-comment claim "rejection text never echoes credential-bearing input" is contradicted by code.
2. **Warn actionable — capability `is_fresh()` state machine contradicts doc-comment** (`oci/referrer/capability.rs:181-186` + test at `:507-518`). Doc says rewound clock forces reprobe; test `is_fresh_handles_probed_at_in_future` asserts future-dated probe = fresh. Code matches test, not doc. Future-dated `probed_at` (corrupt or adversarial cache file) pins capability indefinitely → DoS on capability detection. **Fix:** compute `now.duration_since(probed_at)` and treat `Err(_)` (probed_at in future) as stale; rewrite the unit test.
3. **Warn actionable — `write_cache` not Windows-safe over existing target** (`oci/referrer/capability.rs:124-139`). `tempfile::NamedTempFile::persist` does not replace-existing on Windows. Second cache write for same registry will fail after TTL expires. **Fix:** `std::fs::rename` after write-to-temp, or replace-existing atomic pattern. Add regression test for double-write.
4. **Warn actionable — `trust_root.rs` PEM parser accepts empty SEQUENCE + trailing garbage**. Only checks DER starts with SEQUENCE tag and declared length ≤ buffer; accepts `MAA=` (empty SEQUENCE) and trailing bytes. **Fix:** require exact DER consumption (`total_declared == der.len()`), reject empty SEQUENCEs, prefer real X.509 parser (`x509-parser` / `der` crate).
5. **High deferred** — fake Sigstore Rekor v1 canonicalisation gap (overlaps Cluster C of RCA; same root as the existing TODO in `fake_sigstore.py:604-616`).

Skipped / dropped: 3 prior-reviewed items, 0 stated-convention, 0 trivia.

---

## Root-Cause Analysis (clusters of >Suggest findings)

Full RCA persisted at `.claude/artifacts/review_r3_rca_oci_referrers.md`. Five clusters:

- **A: Phase 5c stubs reachable from release binary** (T2 + R-RUST-2 + tests #8/#9). Systemic fix: `kind_detail()` method on `ClassifyErrorKind`; replace `unimplemented!()` in reachable CLI paths with typed error returns.
- **B: One-way-door architectural decisions deferred past stub phase** (Arch B1, F1, F2, F3, G1; Researcher BLOCK-1, BLOCK-2). Systemic fix: one ADR amendment cycle before Phase 5c branch creation; add "ADR Amendments Pending" field to plan template Status block.
- **C: Verify pipeline = highest-risk surface, least-specified contracts** (Sec H1, H2, H4; tests #3; Researcher HIGH-4; Spec G2). Systemic fix: pre-Phase 5c contract tests pinning byte-equal identity match, subject-digest binding, SET cross-check fields; verify pipeline panic-message inline test.
- **D: Docs and ADR drift from code without amendment trail** (doc-review Critical + 2 High + 2 Medium + Warn; Researcher WARN-4, WARN-5, HIGH-1, HIGH-2, HIGH-3; Architect G3). Systemic fix: treat ADR amendment as Block-tier in pre-finalize review when diff touches surfaces ADR names; pin code constants to ADR §id in inline doc-comments.
- **E: TOCTOU + permissions hardening on token file** (Sec B1). Systemic fix: `O_NOFOLLOW` + `euid` check + symlink-rejection doc.
- **Codex-only cluster:** parse-before-sanitize anti-pattern in `endpoint.rs` (raw input formatted into error before security cleanup) + test-vs-doc contradiction in capability cache state machine + platform-specific atomic-write footgun + PEM laxness in trust-root. Root: lack of negative-case test coverage for "could rejection text echo credentials?" / "what does this do on Windows?" / "what does this accept?"

---

## Deferred Findings (human judgment required)

Twenty deferred findings across reviewers. Highest-priority decisions blocking Phase 5c:

1. **Rekor v2 ownership** — open sigstore-rs upstream issue + add tracking issue to OCX repo + amend ADR Risks. Without this, OCX is invisible to Sigstore's client-upgrade gate.
2. **TUF rotation SLA** — embedded-only (with documented "must upgrade within 90 days of any sigstore-trust-root release containing root rotation") vs embedded + `from_tuf()` refresh. Phase 5c cannot ship with `EmbeddedAssetMissing` as the production state.
3. **`Client::transport` accessor pattern** — pick option (1) public accessor, (2) `SignContext::new_from_client(&Client)`, or (3) `SignPipeline::run(&Client, options)` and amend ADR §"Context injection" before Phase 5c.
4. **`state/` tier in three-store architecture** — amend ADR + `arch-principles.md` + `subsystem-file-structure.md` to acknowledge fourth tier, or relocate capability cache to existing tier.
5. **Capability cache TTL** — reconcile 6h flat (code) vs 24h CI / 1h interactive (ADR/FR-17). Either update ADR to "6h flat; CI split deferred to v2" or implement the split.
6. **Symlink policy on `--identity-token-file`** — reject all symlinks (CWE-367 safest) vs allow if resolved target passes permission check (CI patterns like `/secrets/->/run/secrets-mount`).
7. **Offline verify exit code** — 81 (OfflineBlocked, consistent with global semantics) vs new `OfflineVerifyRefused → 77` (consistent with sign).
8. Plus 13 narrower items (FR-15 cosign interop runtime, NFR-5 token-leak grep test infra, MAX_BUNDLE_SIZE rationale anchor, JWT algorithm allowlist, manual S3 marker convention, …).

---

## Plan Status

Mutating `current_plan.md`/Status not applicable — no `current_plan.md` present and no plan-level Status block exists for this review run (per skill protocol, mutate only when present).

---

## Handoff

Recommended next step: **`/swarm-execute max "apply max-tier review round-3 findings on feat/oci-referrers-sign-verify"`** with the round-3 findings list as input. The execute pass should:

1. **Block-tier fixes (must land):**
   - Security B1 — `O_NOFOLLOW` on `--identity-token-file` open
   - Codex #1 — sanitize userinfo before formatting in `validate_sigstore_url` parse-error
   - Codex #2 — fix `is_fresh()` state machine + test
   - Codex #3 — Windows-safe atomic rename in `write_cache`
   - Codex #4 — tighten PEM/DER acceptance in `trust_root.rs`
2. **Doc Critical fix:** move `package verify` heading hierarchy.
3. **Spec T2 fix:** add `kind_detail()` method + populate `error.detail` in envelope.
4. **ADR amendment cycle (deferred → `/architect`):** B1 / F1 / G1 / BLOCK-1 / BLOCK-2 / TTL drift / state/ tier.
5. **Phase 5c pre-flight checklist additions:** SET cross-check fields; bundle annotation; cert chain length/size caps; subject-digest binding.

For SOTA gaps requiring own ADR (Rekor v2 process, TUF rotation SLA), escalate to **`/architect "propose ADR amendments for OCI referrers signing v1: Rekor v2 client tracking + TUF rotation SLA"`**.

For doc fixes alone, **`/builder docs`** target `command-line.md`, `environment.md`, `signing.md`.
