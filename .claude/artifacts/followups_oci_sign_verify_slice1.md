# Follow-ups — OCI sign/verify slice-1 landing (for owner review)

Parking lot for items found during the autonomous landing/review of `feat/oci-referrers-sign-verify`
that are **deliberately NOT done on this branch** (out of slice-1 scope). Nothing here blocks landing
slice-1; each is a future-feature decision for the owner.

## Out-of-scope review findings (deferred, not implemented)

### 1. Lift `endpoint` to `oci::endpoint` (ADR Amendment 2)
- **What:** `oci::verify::error` and `oci::sign::error` both import `UrlRejection` / `validate_sigstore_url`
  from `crate::oci::sign::endpoint` — `verify` structurally depends on `sign`'s submodule for a shared
  URL validator. ADR `adr_oci_referrers_signing_v1.md` **Amendment 2** already records the decision to
  lift this to a sibling `oci::endpoint` module.
- **Why deferred:** It is a mechanical refactor the ADR explicitly files under **Phase 5c impact**
  (slice-2: TSA endpoint URLs need the same validator). The current code compiles, is fully tested, and
  has zero behaviour impact — `sign` and `verify` are sibling modules in the same crate, so there is no
  real isolation problem. Doing the move now is scope creep with rebase risk for no slice-1 benefit.
- **When:** Do it as the first step of slice-2 (sigstore-rs pipeline) work, alongside adding the TSA
  validator. Tracked by ADR Amendment 2; no separate GitHub issue needed.

## Recordings deferred (important — deviation from the "add website recordings" ask)

Slice-1 `ocx package sign` / `ocx package verify` are an **intentional scaffold**: `execute()` validates
inputs (flag parsing, offline rejection, OIDC token resolution + identity-token-file permission checks)
and then returns a typed error that classifies to **exit 78** (`pipeline_pending` / `trust_root_unavailable`).
The actual Sigstore pipeline (`SignPipeline::run` / `VerifyPipeline::run`) is `unimplemented!()` and never
reached — deferred to slice-2 / Phase 5c.

Consequence: there is **no working happy-path to record**. A cast of `ocx package sign …` would show it
exit 78 ("pipeline not yet wired"), which would be a misleading "feature demo" on the website. I therefore
**did not fabricate a recording** of working signing. The docs now accurately describe the preview state
(see fixes below). Build the real sign/verify website cast when the slice-2 pipeline lands — the test
harness is ready: `test/tests/fixtures/fake_sigstore.py::FakeSigstoreStack` is a context manager that
starts real fake Fulcio/Rekor/OIDC servers, so a `test/doc_scripts/authoring__package-sign.sh` scenario
backed by a setup that runs the stack will produce a genuine cast at that point.

## Slice-2 / future work already tracked in GitHub issues
- #106 — Registry referrers capability detection (clean error, no tag fallback) — the capability probe the
  CLI doesn't yet reach.
- #107 — Upgrade `sigstore-rs` to Rekor v2 / TUF-distributed trust root — the deferred pipeline + TSA work.
- #99  — Auto-verify on `ocx install` / `ocx pull` (the "preview: standalone command only" note points here).
- #98  — Identity-pinned verify (`[trust.policy]` in `ocx.toml`).
- #100 / #101 — SBOM attach / discovery. #103 — SLSA provenance verify. #105 — mirror re-sign/attest.
- #24  — parent referrers ADR.

The core slice-2 deliverable (implement `SignPipeline::run` / `VerifyPipeline::run` via sigstore-rs) maps
to #107 + #106. If the owner wants a dedicated "implement slice-2 sign/verify pipeline" umbrella issue,
that is a quick create — flagged here rather than auto-created (proposal-first).

## Review caveat
The adversarial review's verification phase was partially server-side rate-limited (≈22 verify sub-agents
returned `Rate limited`), so some correctness/test-dimension findings went unverified and were filtered out
rather than confirmed. The 9 confirmed in-scope findings were all addressed; `task rust:verify` (2522 unit
tests) is green, which independently validates Rust correctness. A second lightweight review pass over the
final diff is cheap insurance if the owner wants belt-and-suspenders before merge.
