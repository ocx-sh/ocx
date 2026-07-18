# Code Review: feat/signing-and-trust (tier=max, adversarial)

## Summary

- **Verdict: Request Changes**
- **Tier:** max | **Baseline:** main | **Reviewer model:** fable (8 workers) | **Cross-model:** Codex gpt-5.6-sol (ran)
- **Diff:** 172 files, +28,467 / −206, ≥8 subsystems (~85 code files; ~60 `.claude/artifacts/*` read as context only)
- **Context:** plan `plan_milestone2_signing_trust` is `finalized`; **PR #203 open**; production-hardening → **#205**, website cast → **#204**. Findings below split into *shippable-scope defects* (block PR #203) vs *already-tracked deferrals* (legitimate v1 scope — NOT new blockers).

Why Request Changes: **4 Block** + **~19 actionable Warn**, several **systemic** (≥3 findings share a root), plus **2 architecture boundary violations** — all independent of the documented Sigstore-fidelity deferrals. The deferrals themselves are honest and tracked; they are not what forces the verdict.

Codex agreement: cross-model independently confirmed the strongest security signal (SET/body not bound to bundle → expired-key splice) and the referrer-selection gap — both surfaced by the Claude panel too. Codex's other two "high" findings are the *documented* #197/#205 deferrals re-surfaced (Codex does not treat the deferral docs as binding).

---

## Block-tier (shippable scope — block PR #203)

| # | Finding | Anchor | Sources |
|---|---|---|---|
| B1 | **trust.rs scope rustdoc asserts the OPPOSITE of code+test+ADR** and schemars publishes it into `config/v1.json` / `project/v1.json` — operators reading the schema believe `tool` covers `tool-cli`; uncovered package then installs with only an INFO log. Security-contract doc lie. | `trust.rs:48-49` (+ stale module doc `:12-20`) | r2-arch B1, r1-spec #2, r2-docs #10/#11, r2-quality (W-class) |
| B2 | **`PackageManager::sign_one` is a pub facade whose body is `unimplemented!()`** — zero callers, `ocx_lib` vendored by ocx-mirror → reachable panic on public surface. Dead `SignOptions`/`SignReport`/`_map_sign_error`/`SignErrorKind::PipelinePending` + vacuous test alongside. | `tasks/sign.rs:72-83`, `sign/error.rs:169` | r2-quality B2, r2-arch W3 |
| B3 | **Design-clause labels render in clap `--help`** ("C-S1-3 injection seam", "(C-S1-4, highest precedence)") — the help-hygiene gate regex misses `C-S\d`. User-facing jargon today. | `package_sign.rs:44/48/52/68`, `verify.rs:86` | r2-quality B1 |
| B4 | **Blocking `path.is_dir()` (sync `fs::metadata`) in async `resolve_trust_root`** — fires on every `--tuf-root`/`OCX_SIGSTORE_TUF_ROOT` resolution. quality-rust Block: no std::fs in async. | `verify/trust_resolve.rs:98` | r2-perf #1 |

---

## Security (verify-path hardening — merge-relevant)

- **[Block-adjacent, CWE-400] Unbounded in-memory bundle-blob read from untrusted registry.** `pull_blob` streams the full body with no cap; the 512 KiB `parse_bundle` cap runs *after* download. Bundle-blob digest comes from the untrusted referrer manifest, so digest verification does not bound size → attacker registry serves multi-GB blob → OOM/DoS. Fires on **every** auto-verify (pre-install) and `ocx package verify`. This is a NEW download boundary lacking the pre-download caps the layer path carefully applies. **Fix:** reject `bundle_layer.size > MAX` before `pull_blob` (size already in descriptor) and/or `.take(MAX+1)` probe. `verify/pipeline.rs:142-146` — r2-security #1 + r2-perf #5.
- **[Block-adjacent] No reqwest timeout on 4 Sigstore call sites → fail-closed gate becomes fail-hung.** `reqwest::Client::new()` has no default timeout; a stalled Rekor hangs verify forever, and via auto-verify hangs every policy-covered install. Each call also builds a fresh client (no pool reuse). **Fix:** one shared client with `.connect_timeout` + `.timeout`. `verify/pipeline.rs:399`, `sign/fulcio.rs:123`, `sign/rekor.rs:144`, `sign/oidc_ambient_inline.rs:65` — r2-perf #2.
- **[Codex high — CONFIRMED, split] SET/transparency-body not bound to bundle → expired-key splice.** SET is verified over the opaque `canonicalized_body`, but the hashedrekord body is never parsed to confirm its digest/signature/cert match the bundle fields; cert validity is not checked against Rekor `integrated_time`. A leaked *expired* ephemeral Fulcio key can sign a new malicious subject and attach any previously-valid SET/body — all three pass independently. Exposure bounded (ephemeral keys never persisted), but it defeats the short-lived-cert + transparency guarantee. This is the GHSA-whqx-f9j3-ch6m class Sigstore itself shipped twice. Cert-time half is documented→#205; **the body↔bundle cross-check is NOT in signing.md limitations and must be named in #205 + regression-tested.** `verify/pipeline.rs:152-201` — Codex #1 + r2-security #2 + r2-sota §1.

---

## Architecture — boundary violations (verdict-relevant)

- **[Warn, boundary] ADR Amendment 1 shipped as the explicitly *not-recommended* option, undocumented.** Amendment 1 chose Option 3 (`SignPipeline::run(&Client)`, transport `pub(crate)`); shipped Option 1 — `pub fn transport()` + `&dyn OciTransport` in `SignContext`/`VerifyContext`/`VerifyOptions`. Freezes the 13-method `OciTransport` shape into ocx_lib's public API (ocx-mirror vendors it) — expensive to reverse **post-1.0, cheap now**. No amendment records the reversal (the exact "silent defer" Amendment 8 exists to prevent). `oci/client.rs:88` + pipelines — r2-arch W1. **Decide pre-1.0:** refactor to Option 3, or append an acceptance amendment.
- **[Warn, boundary] Both CLI commands bypass the PackageManager facade.** `command/verify.rs:157-168` hand-builds `VerifyContext` + calls `VerifyPipeline::run` directly, duplicating `tasks/verify.rs:78-100`; sign side drives `SignPipeline::run` at `package_sign.rs:144-156`. Violates command pattern + lib-hosts-substance; two assembly sites must stay field-for-field. Fixing routes through `verify_one`/`sign_one` (also resolves B2). r2-arch W2.

---

## Other actionable Warn (grouped)

- **Perf N-scaling (r2-perf #3/#4/#6):** under a PEM trust root the ladder never consults the cache → N identical Rekor-key fetches per invocation; trust-cache rewritten after every successful verify (50 pkgs = 50 concurrent tempfile+rename on one file; slides 24h TTL on use); capability-probe thundering herd on cold cache (up to N concurrent probes+writes of one file). Memoize fetched key per invocation; skip cache write when content-equal; singleflight the probe by registry.
- **Test-effect gaps (r1-tests #1-#4):** idempotency test passes on double-publish (asserts rc only, not referrer count); offline-success tests pass if verification silently skipped (add warm-cache wrong-identity→77); sign-path Rekor-503→83 and token-rejection→80 never driven E2E; `ocx run` gate surface untested (4/6 surfaces pinned). subsystem-tests gap: "would this test pass if the feature were a no-op?"
- **Doc-as-contract drift (r2-docs, 9 Warn):** slug example wrong (`ghcr.io`→`ghcr.io.json`, dots preserved); cache-field table wrong twice (`"supported"` snake_case; `probed_at` is SystemTime struct); hard-fail condition inverted (404/405→84, other errors own class); `--rekor-url` https rule mis-stated on verify row; nonexistent `GOOGLE_OAUTH_TOKEN` cited for ambient CI; reference verify examples fail exit-78 copy-paste (missing `--tuf-root`); scope "segment-boundary" blanket false for mid-string `*`.
- **CLI quality (r2-quality W1-W6):** shared `options::Verify` help misleads on install/pull (no signature-verification wording, no `OCX_NO_VERIFY` note); `OCX_NO_VERIFY` read from env 3× (SoT); `remediation` envelope field documented but hardcoded `None`; `package_sign.rs`/`verify.rs` naming inconsistent (→`package_verify.rs`); missing `.with_context` on project-config read.

---

## Root-Cause Analysis (clusters ≥3 findings)

- **Cluster A — Scaffolding residue after critical-path wiring** (B2, r2-arch W3/S2). #194 wired pipelines but left the facade stub, dead error variant, stale "Phase 1 scaffolding" docs. Root: no issue-close gate grepping `unimplemented!` + phase-docs in touched modules. **Systemic fix:** add that grep to close checklist; route CLI through facade.
- **Cluster B — Doc-as-contract drift** (B1, B3, r1-spec #1/#2, all r2-docs, r2-quality W4). Docs written at design time, never re-verified post-impl; schemars turns rustdoc into a published *security* contract not treated as one; help-hygiene regex too narrow. **Systemic fix:** extend help gate to `C-S\d`; post-impl step cross-checking schemars-visible rustdoc + website claims against test assertions; treat `[[trust.policy]]` doc surfaces as security-reviewed.
- **Cluster C — New network/IO boundary lacks the hardening the old one has** (bundle size cap, reqwest timeouts, N-fetch/write amplification, sync-io-async). Verify/sign hand-rolled against `fake_sigstore.py`, which is never hostile on size and never stalls; subsystem-oci cap discipline documented for *layers* only. **Systemic fix:** one Sigstore-services HTTP seam (shared client + timeouts); generalize the size-cap rule to all transport reads; add a 50-package auto-verify scenario.
- **Cluster D — Tests assert exit code, not effect** (r1-tests #1-#4). Contract-table exit codes cheaper to assert than effects; failure-injection wired on verify only. **Systemic fix:** add the no-op question to subsystem-tests.md; parametrize gate tests over the 6-surface design-spec matrix.
- **Cluster E — Decision recorded ≠ decision shipped** (r2-arch W1/W2/W4, r1-spec #3, Codex #4). ADR amendment loop requires manual closure; per-issue spec-compliance traced acceptance criteria, not amendment-level API shape; multi-referrer semantics fell between issues. **Systemic fix:** amendment→commit traceability row in the spec-compliance reviewer prompt; decide ANY-of vs first-referrer now.

---

## Cross-Model Adversarial (Codex gpt-5.6-sol) — triage

| Codex finding | Class | Disposition |
|---|---|---|
| #1 Rekor proof splice on expired key (`pipeline.rs:152-201`) | **Actionable (split)** | Cert-time → #205 (tracked). **Body↔bundle cross-check → NOT documented; add to #205 + regression test.** Triple-confirmed (r2-security #2, r2-sota §1). |
| #2 Private `ocx-rekor-set-v1` SET, public Rekor won't sign it (`rekor.rs:180-197`) | **Deferred (tracked)** | Documented #197 cosign interop + signing.md "blocked, not merely unwired". Not a new blocker; it is the declared v1 scope boundary. |
| #3 Clean install can't bootstrap; operator-policy-without-trust-root → all covered installs exit 78 (`trust_root.rs:130-137`) | **Deferred + surface** | Embedded-root stub → #205 (tracked). **But the operational consequence deserves a prominent docs warning now** (auto-verify + policy but no configured root fails every covered install by default). |
| #4 First-referrer-only, ordering not policy-tied (`pipeline.rs:119-150`) | **Actionable (document + decide)** | Quadruple-confirmed (r2-arch W4, r2-security #4, r1-tests D1). ADR-decide ANY-of (cosign) vs first-only; add to signing.md limitations; add malformed-first-referrer + rotation tests. Fail-closed → availability DoS, not forgery. |

Codex verdict "NO-SHIP" reflects its (correct, from a cold read) refusal to accept the SET/embedded-root deferrals as security guarantees. Two of its four highs ARE those documented deferrals — legitimate v1 scope. The one net-new security escalation (SET-body binding, #1) and the referrer-selection gap (#4) are folded into the actionable set above.

---

## Deferred (legitimate — tracked or design-owned, NOT merge blockers)

- Real Sigstore fidelity: private SET format, embedded TUF root stub, cert temporal binding, TOFU Rekor key — **#205** (production hardening) + #197 (cosign interop). Honestly documented in signing.md / ADRs / plan; r1-spec verified no overclaims.
- Auto-verify opt-in posture (uncovered pkg → proceeds) — documented design (adr_trust_policy). Consider an enforce-all / `require-signed` mode to close the transitive-dep hole (r2-sota #3).
- trust_cache ↔ referrer/capability duplication — deliberate mirror per subsystem-file-structure; extract on the *third* TTL-JSON cache (r2-arch S1).
- Offline air-gap: covered package can't install offline even when locally present (signature material always from registry) — matches ADR scoping (r2-perf #9).
- SOTA/positioning (r2-sota): GHCR (primary persona's default registry) can't host signed packages by design (exit 84) — watch community #163029; keep differentiator #12 wording scoped to "signing" not "provenance" until DSSE (#198) lands.

---

## Handoff

Actionable findings → **`/swarm-execute max "apply signing-and-trust review findings"`** (review-fix loop). Suggested fix order (cheap→structural):
1. B1/B3 + r2-docs drift + rustdoc — doc/schema truth pass (regen schemas).
2. B4 + bundle size cap + reqwest timeouts — verify-path hardening (merge-relevant security/perf).
3. B2 + r2-arch W2 — route CLI through `verify_one`/`sign_one` (kills the panic stub + facade bypass together).
4. r2-arch W1 — ADR Amendment 1: refactor to Option 3 (cheap pre-1.0) or record acceptance amendment. **Owner decision.**
5. Codex #4 / r2-arch W4 — decide ANY-of vs first-referrer; document + test. **Owner decision.**
6. Codex #1 body-binding + #3 bootstrap-consequence → extend **#205** scope + add regression tests / docs warning.

Test gaps (Cluster D) fold into whichever fix touches their surface. No auto-fixes applied — review is read-only.
